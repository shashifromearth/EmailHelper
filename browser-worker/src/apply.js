// POST /v1/apply/inspect and POST /v1/apply/fill
import fs from 'node:fs/promises';
import path from 'node:path';
import { readFieldStates, markSubmitButton } from './dom/discover.js';
import { detectBlocker, getMainStatus } from './blockers.js';
import { openApplication, discoverAll, detectAts, publicField, pageUrl } from './navigation.js';
import { resolveUpload } from './uploads.js';
import { pollUntil, sleep } from './sync.js';

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable)
// ---------------------------------------------------------------------------

const normKey = (s) =>
  String(s ?? '')
    .toLowerCase()
    .replace(/[_\-\s]+/g, ' ')
    .replace(/[?:*\u2731.]+$/g, '')
    .trim();

export const cssString = (s) => '"' + String(s).replace(/["\\]/g, '\\$&').replace(/\n/g, '\\a ') + '"';

/**
 * Map answer keys to fields. Precedence: exact field id, case-insensitive id,
 * normalised label (case/whitespace/underscore-insensitive, trailing ?:* ignored).
 * With `fuzzy` (used for `files`), a key also matches a field whose id/label
 * contains it, and "resume"/"cv" match each other.
 * @returns {{ matches: Map<string, {key: string, value: any}>, unmatched: string[] }}
 */
export function matchKeys(fields, obj, { fuzzy = false, types = null } = {}) {
  const pool = types ? fields.filter((f) => types.includes(f.type)) : fields;
  const matches = new Map();
  const unmatched = [];
  const keys = Object.keys(obj || {});
  const take = (f, key) => matches.set(f.id, { key, value: obj[key] });
  const free = (f) => !matches.has(f.id);
  const remaining = [];
  for (const key of keys) {
    const f = pool.find((x) => x.id === key && free(x));
    if (f) take(f, key);
    else remaining.push(key);
  }
  for (const key of remaining) {
    const nk = normKey(key);
    let f =
      pool.find((x) => free(x) && x.id.toLowerCase() === key.toLowerCase()) ||
      pool.find((x) => free(x) && normKey(x.label) === nk);
    if (!f && fuzzy && nk.length >= 2) {
      const syn = /^(resume|cv|resume cv)$/.test(nk) ? /resume|\bcv\b|curriculum/i : null;
      f =
        pool.find((x) => free(x) && (normKey(x.id).includes(nk) || normKey(x.label).includes(nk))) ||
        (syn && pool.find((x) => free(x) && (syn.test(x.id) || syn.test(x.label))));
    }
    if (f) take(f, key);
    else unmatched.push(key);
  }
  return { matches, unmatched };
}

/** Pick the option matching an answer: exact label, exact value, then label-contains (shortest wins). */
export function chooseOption(opts, answer) {
  if (answer === true) answer = 'Yes';
  if (answer === false) answer = 'No';
  const a = String(answer ?? '').replace(/\s+/g, ' ').trim().toLowerCase();
  if (!a) return null;
  const lab = (o) => o.label.replace(/\s+/g, ' ').trim().toLowerCase();
  return (
    opts.find((o) => lab(o) === a) ||
    opts.find((o) => String(o.value).toLowerCase() === a) ||
    opts.filter((o) => lab(o).includes(a)).sort((x, y) => x.label.length - y.label.length)[0] ||
    null
  );
}

export function isTruthy(v) {
  if (typeof v === 'boolean') return v;
  if (typeof v === 'number') return v !== 0;
  return /^(true|yes|y|on|1|checked|agree|i agree|accept)$/i.test(String(v ?? '').trim());
}

const toText = (v) => {
  if (Array.isArray(v)) return v.join(', ');
  if (typeof v === 'boolean') return v ? 'Yes' : 'No';
  return String(v);
};

export function screenshotName(sessionKey, now = Date.now()) {
  return `${String(sessionKey).replace(/[^A-Za-z0-9_.-]/g, '_').slice(0, 100)}-${Math.floor(now / 1000)}.png`;
}

// ---------------------------------------------------------------------------
// Browser-side actions
// ---------------------------------------------------------------------------

const fieldLoc = (frame, id) => frame.locator(`[data-eh-field=${cssString(id)}]`);
const optionLoc = (frame, id, index) => frame.locator(`[data-eh-field=${cssString(id)}][data-eh-opt="${index}"]`);

async function setChecked(loc, want) {
  try {
    await loc.setChecked(want, { timeout: 3000 });
  } catch {
    // hidden custom-styled inputs: fall back to a DOM click / direct set
    await loc.evaluate((el, w) => {
      if (el.checked !== w) el.click();
      if (el.checked !== w) {
        el.checked = w;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
      }
    }, want);
  }
}

async function fillText(loc, text) {
  try {
    await loc.fill(text, { timeout: 5000 });
  } catch (e) {
    if (/Cannot type|Malformed value|not an <input>/i.test(e.message)) throw e;
    await loc.evaluate((el, t) => {
      const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, t);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
    }, text);
  }
}

/** Fill one non-file field. Returns null on success or a reason string. */
async function fillField(frame, field, value) {
  if (value === null || value === undefined) return 'no value';
  const loc = fieldLoc(frame, field.id).first();
  switch (field.type) {
    case 'select': {
      const values = Array.isArray(value) ? value : [value];
      const picks = [];
      for (const v of values) {
        const o = chooseOption(field._opts, v);
        if (!o) return `no option matching "${toText(v)}" (options: ${field.options.join(' | ')})`;
        picks.push(o);
      }
      const chosen = field._multiple ? picks : picks.slice(0, 1);
      await loc.selectOption(chosen.map((o) => ({ index: o.index })), { force: true, timeout: 5000 });
      return null;
    }
    case 'radio': {
      const o = chooseOption(field._opts, Array.isArray(value) ? value[0] : value);
      if (!o) return `no option matching "${toText(value)}" (options: ${field.options.join(' | ')})`;
      await setChecked(optionLoc(frame, field.id, o.index), true);
      return null;
    }
    case 'checkbox': {
      if (!field._group) {
        await setChecked(loc, isTruthy(Array.isArray(value) ? value.length > 0 : value));
        return null;
      }
      let wanted;
      if (typeof value === 'boolean') {
        if (field._opts.length !== 1 && value) return 'checkbox group needs an array of option labels';
        wanted = value ? field._opts.slice(0, 1) : [];
      } else {
        wanted = [];
        for (const v of Array.isArray(value) ? value : [value]) {
          const o = chooseOption(field._opts, v);
          if (!o) return `no option matching "${toText(v)}" (options: ${field.options.join(' | ')})`;
          wanted.push(o);
        }
      }
      const want = new Set(wanted.map((o) => o.index));
      for (const o of field._opts) await setChecked(optionLoc(frame, field.id, o.index), want.has(o.index));
      return null;
    }
    case 'file':
      return 'file fields are set from `files`';
    default:
      await fillText(loc, toText(value));
      return null;
  }
}

/** Set a file input and verify the upload. Returns null on success or a reason. */
async function uploadFile(frame, field, filePath, cfg, ats) {
  const res = await resolveUpload(filePath, cfg.allowedUploadDirs);
  if (!res.ok) return res.reason;
  const loc = fieldLoc(frame, field.id).first();
  await loc.setInputFiles(res.path, { timeout: 10000 });
  const name = path.basename(res.path);
  const sel = `[data-eh-field=${cssString(field.id)}]`;
  const check = () =>
    frame.evaluate(
      ([s, n]) => {
        const el = document.querySelector(s);
        return {
          inInput: !!(el && el.files && [...el.files].some((f) => f.name === n)),
          inPage: ((document.body && document.body.innerText) || '').includes(n),
        };
      },
      [sel, name],
    );
  const first = await check().catch(() => ({ inInput: false, inPage: false }));
  if (first.inPage) return null;
  if (first.inInput) {
    // ATS UIs echo the filename shortly after upload; give them a moment (not fatal if absent)
    if (['greenhouse', 'lever', 'ashby', 'workday'].includes(ats)) {
      await pollUntil(async () => (await check()).inPage, { timeoutMs: 2000, intervalMs: 200 });
    }
    return null;
  }
  // some ATS move the file elsewhere (async upload) and clear the input; wait for the name to show
  const later = await pollUntil(async () => {
    const x = await check();
    return x.inPage || x.inInput ? x : null;
  }, { timeoutMs: 5000, intervalMs: 250 });
  return later ? null : `file "${name}" did not show up in the input or on the page after upload`;
}

/** One pass over `fields`, filling everything that has an answer and is not done yet. */
async function fillPass(frame, fields, req, cfg, ats, state) {
  const unused = (obj, prefix) =>
    Object.fromEntries(Object.entries(obj || {}).filter(([k]) => !state.usedKeys.has(`${prefix}:${k}`)));
  const pending = fields.filter((f) => !state.done.has(f.id));
  const files = matchKeys(pending, unused(req.files, 'files'), { fuzzy: true, types: ['file'] });
  const answers = matchKeys(pending, unused(req.answers, 'answers'));
  for (const f of fields) {
    if (state.done.has(f.id)) continue;
    let m = null;
    let isFile = false;
    if (f.type === 'file') {
      m = files.matches.get(f.id) || answers.matches.get(f.id);
      isFile = true;
    } else {
      m = answers.matches.get(f.id);
    }
    if (!m) continue;
    state.usedKeys.add(`${isFile && files.matches.get(f.id) ? 'files' : 'answers'}:${m.key}`);
    state.done.add(f.id);
    try {
      const reason = isFile ? await uploadFile(frame, f, m.value, cfg, ats) : await fillField(frame, f, m.value);
      if (reason) {
        state.skipped.push({ key: m.key, field_id: f.id, reason });
        if (isFile && !state.uploadFailure) state.uploadFailure = `${f.id}: ${reason}`;
      } else {
        state.filled.push(f.id);
        if (isFile) state.uploaded.add(f.id);
      }
    } catch (e) {
      const reason = e.message.split('\n')[0].slice(0, 300);
      state.skipped.push({ key: m.key, field_id: f.id, reason });
      if (isFile && !state.uploadFailure) state.uploadFailure = `${f.id}: ${reason}`;
    }
  }
}

function findConfirmation(knownLines) {
  const RE = /thank you|thanks for applying|application (has been )?(received|submitted)|we('| ha)ve received/i;
  const text = (document.body && document.body.innerText) || '';
  const known = new Set(knownLines || []);
  const lines = text
    .split('\n')
    .map((l) => l.replace(/\s+/g, ' ').trim())
    .filter((l) => l && RE.test(l));
  if (!knownLines) return lines;
  return lines.find((l) => !known.has(l)) || null;
}

function headingText() {
  const h = document.querySelector('h1, h2');
  return ((h && h.innerText) || document.title || '').replace(/\s+/g, ' ').trim().slice(0, 300) || null;
}

async function readConfirmation(page, frame, known) {
  for (const f of new Set([page.mainFrame(), frame])) {
    try {
      const t = await f.evaluate(findConfirmation, known);
      if (t) return t.slice(0, 300);
    } catch {
      /* navigating / detached */
    }
  }
  return null;
}

async function submitForm(page, frame, log) {
  const btn = await frame.evaluate(markSubmitButton).catch(() => null);
  if (!btn) return { ok: false, reason: 'submit button not found' };
  const known = await frame.evaluate(findConfirmation, null).catch(() => []);
  const knownMain = frame === page.mainFrame() ? known : await page.mainFrame().evaluate(findConfirmation, null).catch(() => []);
  const knownAll = [...known, ...knownMain];
  const before = page.url();
  log.info('clicking submit', { button: btn.text });
  await frame.locator('[data-eh-submit]').first().click({ timeout: 10000 });

  let urlChangedAt = 0;
  const r = await pollUntil(async () => {
    const text = await readConfirmation(page, frame, knownAll);
    if (text) return { text };
    if (page.url() !== before) {
      urlChangedAt ||= Date.now();
      // after the URL changes, allow up to 3 s for confirmation text to render
      if (Date.now() - urlChangedAt > 3000) return { text: null, urlChanged: true };
    }
    return null;
  }, { timeoutMs: 15000, intervalMs: 300 });
  if (r && r.text) return { ok: true, confirmation: r.text };
  if (page.url() !== before) {
    await page.waitForLoadState('domcontentloaded').catch(() => {});
    const d = await discoverAll(page);
    if (d.appLike && d.fields.length > 0) {
      return { ok: false, reason: 'URL changed after submit but an application form is still shown (multi-step form or validation errors)' };
    }
    return { ok: true, confirmation: await page.mainFrame().evaluate(headingText).catch(() => null) };
  }
  return { ok: false, reason: 'no confirmation (URL change or thank-you text) within 15 s after clicking submit' };
}

async function takeScreenshot(page, cfg, key) {
  try {
    await fs.mkdir(cfg.screenshotDir, { recursive: true });
    const name = screenshotName(key);
    await page.screenshot({ path: path.join(cfg.screenshotDir, name), fullPage: true, timeout: 15000 });
    return path.posix.join(cfg.screenshotDirRaw.replace(/\\/g, '/'), name);
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

export async function inspect(mgr, { session_key, url }, log) {
  return mgr.withSession(session_key, async (entry) => {
    const { navError, discovery } = await openApplication(mgr, entry, url, log);
    const page = entry.page;
    const fields = discovery ? discovery.fields : [];
    let blocker = await detectBlocker(page, { navError, fields: discovery ? fields : null, frame: discovery?.frame });
    if (!blocker && fields.length === 0) {
      blocker = { kind: 'unexpected_form', detail: 'no fillable application form found on the page' };
    }
    const finalUrl = pageUrl(page, url);
    const out = { final_url: finalUrl, ats: detectAts(finalUrl, discovery?.markers), fields: fields.map(publicField), blocker };
    if (blocker && fields.length === 0) await mgr.closeSession(session_key, 'blocked');
    log.info('inspect done', { session_key, ats: out.ats, fields: fields.length, blocker: blocker?.kind ?? null });
    return out;
  });
}

const HARD_PRE_FILL = new Set(['network_error', 'site_error', 'access_denied', 'rate_limited', 'login_required', 'otp', 'mfa']);

export async function fill(mgr, req, log) {
  const { session_key, url } = req;
  return mgr.withSession(session_key, async (entry, { fresh }) => {
    const res = {
      status: 'filled',
      final_url: url,
      missing_required: [],
      blocker: null,
      confirmation_text: null,
      screenshot_path: null,
      filled: [],
      skipped: [],
      error: null,
    };
    let close = false;
    try {
      let discovery = null;
      let navError = null;
      if (!fresh && entry.url === url && !entry.page.isClosed()) {
        const d = await discoverAll(entry.page);
        if (d.appLike && d.fields.length > 0) discovery = d;
      }
      if (!discovery) ({ navError, discovery } = await openApplication(mgr, entry, url, log));
      const page = entry.page;

      let blocker = await detectBlocker(page, { navError, fields: discovery ? discovery.fields : null, frame: discovery?.frame });
      if (!blocker && (!discovery || discovery.fields.length === 0)) {
        blocker = { kind: 'unexpected_form', detail: 'no fillable application form found on the page' };
      }
      if (blocker && (HARD_PRE_FILL.has(blocker.kind) || !discovery || discovery.fields.length === 0)) {
        res.status = 'blocked';
        res.blocker = blocker;
        close = true;
        return res;
      }

      const ats = detectAts(page.url(), discovery.markers);
      const state = { done: new Set(), filled: [], skipped: [], uploaded: new Set(), usedKeys: new Set(), uploadFailure: null };
      await fillPass(discovery.frame, discovery.fields, req, mgr.cfg, ats, state);
      // conditional questions can appear after answering others: one more pass
      await sleep(150);
      const d2 = await discoverAll(page);
      const finalD = d2.appLike && d2.fields.length > 0 ? d2 : discovery;
      if (finalD === d2) await fillPass(d2.frame, d2.fields, req, mgr.cfg, ats, state);

      for (const k of Object.keys(req.answers || {})) {
        if (!state.usedKeys.has(`answers:${k}`)) state.skipped.push({ key: k, field_id: null, reason: 'no matching field' });
      }
      for (const k of Object.keys(req.files || {})) {
        if (!state.usedKeys.has(`files:${k}`)) state.skipped.push({ key: `files.${k}`, field_id: null, reason: 'no matching file field' });
      }
      res.filled = state.filled;
      res.skipped = state.skipped;

      const requiredIds = finalD.fields.filter((f) => f.required).map((f) => f.id);
      const states = await finalD.frame.evaluate(readFieldStates, requiredIds).catch(() => ({}));
      res.missing_required = requiredIds.filter((id) => !state.uploaded.has(id) && (!states[id] || states[id].empty));

      blocker = state.uploadFailure
        ? { kind: 'upload_failure', detail: state.uploadFailure }
        : await detectBlocker(page, { fields: finalD.fields, frame: finalD.frame });
      if (blocker) {
        res.status = 'blocked';
        res.blocker = blocker;
        close = true;
      } else if (req.submit === true && res.missing_required.length === 0) {
        close = true; // never reuse a page after a submit attempt
        const sub = await submitForm(page, finalD.frame, log);
        const after = await detectBlocker(page, {});
        if (after && after.kind !== 'login_required') {
          res.status = 'blocked';
          res.blocker = after;
        } else if (sub.ok) {
          res.status = 'submitted';
          res.confirmation_text = sub.confirmation;
        } else {
          const status = getMainStatus(page);
          res.status = 'error';
          res.error = sub.reason + (status ? ` (last HTTP status ${status})` : '');
        }
      }
      return res;
    } catch (e) {
      res.status = 'error';
      res.error = e.message.split('\n')[0].slice(0, 500);
      close = true;
      return res;
    } finally {
      if (!entry.page.isClosed()) {
        res.final_url = pageUrl(entry.page, url);
        res.screenshot_path = await takeScreenshot(entry.page, mgr.cfg, session_key);
      }
      log.info('fill done', {
        session_key,
        status: res.status,
        blocker: res.blocker?.kind ?? null,
        missing_required: res.missing_required.length,
        filled: res.filled.length,
      });
      if (close) await mgr.closeSession(session_key, res.status);
    }
  });
}
