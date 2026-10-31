// Pure decisions, kept free of chrome.* so they run under `node --test`.

/** Pick the option matching an answer: exact label, exact value, then label-contains (shortest wins). */
export function chooseOption(opts, answer) {
  if (answer === true) answer = 'Yes';
  if (answer === false) answer = 'No';
  const a = norm(answer).toLowerCase();
  if (!a) return null;
  const lab = (o) => norm(o.label).toLowerCase();
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
  return /^(true|yes|y|on|1|checked|agree|i agree|accept)$/i.test(norm(v));
}

export function toText(v) {
  if (Array.isArray(v)) return v.join(', ');
  if (typeof v === 'boolean') return v ? 'Yes' : 'No';
  return String(v ?? '');
}

const norm = (s) => String(s ?? '').replace(/\s+/g, ' ').trim();
const list = (v) => (Array.isArray(v) ? v : [v]);

/** What the agent needs to know about a field (the `_` keys stay in the extension). */
export function publicField(f) {
  const out = { id: f.id, label: f.label, type: f.type, required: !!f.required, options: f.options || [] };
  if (f.accept) out.accept = f.accept;
  return out;
}

/** The job a queued link describes, as the agent's `JobDetails`. */
export function jobFromLink(link) {
  if (!link) return {};
  return {
    title: link.title || '',
    company: link.company || null,
    url: link.url || null,
    location: link.location || null,
    remote: link.remote && link.remote !== 'unknown' ? link.remote : null,
    salary: link.salary || null,
    skills: link.skills || [],
    description: link.description || null,
  };
}

/**
 * Turn the agent's answers into in-page fill steps.
 * Returns { steps, resume: [ids], asks: [{id, label, reason}], problems: [{id, reason}] }.
 * A step is { id, type, text } for typed fields, { id, type, indexes } for
 * choices, { id, type, checked } for a single checkbox, { id, type, buttons, indexes } for button groups.
 */
export function buildPlan(fields, answers) {
  const byId = new Map(fields.map((f) => [f.id, f]));
  const out = { steps: [], resume: [], asks: [], problems: [] };
  for (const a of answers) {
    const f = byId.get(a.id);
    if (!f) continue;
    if (a.kind === 'resume') {
      if (f.type === 'file') out.resume.push(f.id);
      continue;
    }
    if (a.kind === 'ask') {
      out.asks.push({ id: f.id, label: f.label, reason: a.reason || '' });
      continue;
    }
    if (a.kind !== 'value' || a.value === null || a.value === undefined) continue;
    const step = stepFor(f, a.value);
    if (typeof step === 'string') out.problems.push({ id: f.id, reason: step });
    else if (step) out.steps.push(step);
  }
  return out;
}

function stepFor(f, value) {
  switch (f.type) {
    case 'file':
      return null; // only the resume is uploaded
    case 'select':
    case 'radio': {
      const want = f.type === 'select' && f._multiple ? list(value) : [list(value)[0]];
      const indexes = [];
      for (const v of want) {
        const o = chooseOption(f._opts || [], v);
        if (!o) return `no option matching "${toText(v)}"`;
        indexes.push(o.index);
      }
      return { id: f.id, type: f.type, indexes, buttons: !!f._buttons };
    }
    case 'checkbox': {
      if (!f._group) return { id: f.id, type: 'checkbox', checked: isTruthy(Array.isArray(value) ? value.length > 0 : value) };
      if (typeof value === 'boolean') {
        if (value && f._opts.length !== 1) return 'checkbox group needs option labels';
        return { id: f.id, type: 'checkbox', indexes: value ? [f._opts[0].index] : [] };
      }
      const indexes = [];
      for (const v of list(value)) {
        const o = chooseOption(f._opts, v);
        if (!o) return `no option matching "${toText(v)}"`;
        indexes.push(o.index);
      }
      return { id: f.id, type: 'checkbox', indexes };
    }
    default:
      return { id: f.id, type: f.type, text: toText(value) };
  }
}

const LOGIN_URL_RE = /login|signin|sign-in|sign_in|authwall|checkpoint/i;

/** Probe signals → { kind, detail } for things only the person can clear, or null. */
export function blockerFrom(probe, url, fields) {
  if (!probe) return null;
  if (probe.cloudflare) return { kind: 'captcha', detail: 'browser check page' };
  if (probe.captcha) return { kind: 'captcha', detail: probe.captcha };
  if (probe.mfa) return { kind: 'mfa', detail: probe.mfa };
  if (probe.otp) return { kind: 'otp', detail: probe.otp };
  if (probe.accessDeniedText) return { kind: 'access_denied', detail: 'page says access denied' };
  if (probe.tooManyText) return { kind: 'rate_limited', detail: 'page says too many requests' };
  let path = String(url || '');
  try {
    const u = new URL(url);
    path = u.hostname + u.pathname;
  } catch {
    /* keep the raw text */
  }
  if (LOGIN_URL_RE.test(path)) return { kind: 'login_required', detail: 'sign-in page' };
  if (probe.passwordVisible && !(fields || []).some((f) => f.type === 'file' || f.type === 'textarea')) {
    return { kind: 'login_required', detail: 'password field and no application fields' };
  }
  return null;
}

/** The frame holding the application form: an app-like one with the most fields. */
export function bestFrame(results) {
  return (
    results
      .filter((r) => r && r.result)
      .sort((a, b) => Number(b.result.appLike) - Number(a.result.appLike) || b.result.fields.length - a.result.fields.length)[0] ||
    null
  );
}

/** Base64 of bytes, chunked so large files do not overflow the call stack. */
export function toBase64(bytes) {
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** What the person answered (see watchSubmit), as text; '' when nothing was given. */
export function answerText(value, options = []) {
  if (typeof value === 'string') return value.trim();
  return (value?.picked || [])
    .filter(Number.isInteger)
    .map((i) => options[i])
    .filter(Boolean)
    .join(', ');
}

// ---- autonomy: auto-submit, escalation backoff, circuit breaker, events ----

/** Job boards: always filled-then-the-person-submits (job-links `Platform::is_assisted`). */
export const JOB_BOARDS = ['linkedin', 'naukri', 'glassdoor', 'wellfound', 'indeed'];
/** Everything else job-links knows: employer career sites and ATSs. */
export const EMPLOYER_PLATFORMS = ['greenhouse', 'lever', 'ashby', 'workday', 'career_site', 'other'];

export const isJobBoard = (platform) => JOB_BOARDS.includes(String(platform || '').toLowerCase());

/** The job board a page is on, from its host (an employer link may redirect to one), else null. */
export function boardForHost(host) {
  const h = String(host || '').toLowerCase();
  const on = (domain) => h === domain || h.endsWith(`.${domain}`);
  if (on('linkedin.com')) return 'linkedin';
  if (on('naukri.com')) return 'naukri';
  if (on('wellfound.com') || on('angel.co')) return 'wellfound';
  if (/(^|\.)glassdoor\.[a-z.]+$/.test(h)) return 'glassdoor';
  if (/(^|\.)indeed\.[a-z.]+$/.test(h)) return 'indeed';
  return null;
}

export const BLOCKER_TEXT = {
  captcha: 'Solve the check on the page',
  mfa: 'Enter the verification code',
  otp: 'Enter the verification code',
  login_required: 'Sign in (or create the account)',
  access_denied: 'The site refused access',
  rate_limited: 'The site is rate limiting; wait a little',
};

/**
 * Platforms to claim: null = any. With auto-submit on and no list set, only
 * employer sites (unattended runs never stall on job boards). Paused
 * platforms are left out; [] means everything allowed is paused.
 */
export function claimPlatforms({ platforms, autoSubmit, paused = [] }) {
  let allow = platforms?.length ? platforms : autoSubmit ? EMPLOYER_PLATFORMS : null;
  if (!allow && !paused.length) return null;
  allow ||= [...EMPLOYER_PLATFORMS, ...JOB_BOARDS];
  return allow.filter((p) => !paused.includes(p));
}

/**
 * Submit on its own only on an employer site, with nothing blocking, a submit
 * button found, no fill problem and no required question left.
 */
export function canAutoSubmit({ platform, asks = [], problems = [], blocker, submitFound, autoSubmit }) {
  return !!(
    autoSubmit &&
    platform &&
    !isJobBoard(platform) &&
    !blocker &&
    submitFound &&
    !problems.length &&
    !asks.some((a) => a.required)
  );
}

/** Why an unattended run needs a person, as { kind, reason }, or null. */
export function stuckReason({ phase, platform, blocker, asks = [], problems = [], submitFound, status }) {
  if (blocker) return { kind: blocker.kind, reason: `${BLOCKER_TEXT[blocker.kind] || blocker.kind} (${blocker.detail})` };
  if (phase === 'error') return { kind: 'error', reason: status || 'error while filling' };
  if (phase === 'waiting') return { kind: 'no_form', reason: 'No application form found on the page' };
  if (phase !== 'review') return null;
  const left = [...asks.filter((a) => a.required), ...problems];
  if (left.length) return { kind: 'questions', reason: `Needs answers: ${left.map((a) => a.label).join(', ')}`.slice(0, 500) };
  if (isJobBoard(platform)) return null;
  if (!submitFound) return { kind: 'no_submit', reason: 'Could not find the submit button' };
  return null;
}

/** Required fields still empty after filling (states from readFieldStates), as asks. */
export function requiredLeft(fields, states, asked = new Set()) {
  return fields
    .filter((f) => f.required && !asked.has(f.id) && states[f.id]?.empty)
    .map((f) => ({ id: f.id, label: f.label || f.id, reason: 'still empty', problem: false }));
}

export const CONFIRM_RE = /thank you|application (has been )?(received|submitted)|we('ve| have) received your application|successfully (applied|submitted)/i;

/** Confirmation text in any frame's readOutcome(). */
export const confirmedText = (outcomes) => outcomes.some((o) => CONFIRM_RE.test(o?.text || ''));

/**
 * After clicking submit: 'confirmed', 'next_step' (form gone, a new one shown)
 * or 'failed', with the validation errors seen. `before` is readOutcome() from
 * just before the click, so a "thank you" already on the page is not proof.
 */
export function submitOutcome(before, after) {
  const errors = [...new Set(after.flatMap((o) => o?.errors || []))];
  if (confirmedText(after) && !confirmedText(before)) return { result: 'confirmed', errors };
  if (after.every((o) => o?.formGone) && !errors.length) {
    return { result: after.some((o) => (o?.inputs || 0) >= 2) ? 'next_step' : 'confirmed', errors };
  }
  return { result: 'failed', errors };
}

const BACKOFF_SECS = [30, 60, 120, 240, 480];
export const BACKOFF_CAP_SECS = 900;
export const GIVE_UP_MS = 2 * 60 * 60 * 1000;

/** Seconds to wait before check number `attempt` (0-based): 30, 60, … 480, then 900. */
export const backoffDelay = (attempt) => BACKOFF_SECS[attempt] ?? BACKOFF_CAP_SECS;

/** When to check an escalation next: the backoff, but never past the give-up time. */
export const nextCheckAt = ({ attempt, startedAt }, now) => Math.min(now + backoffDelay(attempt) * 1000, startedAt + GIVE_UP_MS);

export const shouldGiveUp = ({ startedAt }, now) => now - startedAt >= GIVE_UP_MS;

export const BREAKER_FAILS = 3;
export const BREAKER_PAUSE_MS = 30 * 60 * 1000;

/** Circuit breaker: 3 failures in a row pause a platform for 30 minutes; a success resets it. */
export function recordOutcome(state, platform, ok, now) {
  if (!platform) return state;
  const prev = state[platform] || { fails: 0, pausedUntil: 0 };
  if (ok) return { ...state, [platform]: { fails: 0, pausedUntil: 0 } };
  const fails = prev.fails + 1;
  const next = fails >= BREAKER_FAILS ? { fails: 0, pausedUntil: now + BREAKER_PAUSE_MS } : { fails, pausedUntil: prev.pausedUntil };
  return { ...state, [platform]: next };
}

/** Paused platforms as { platform: until }. */
export function pausedPlatforms(state, now) {
  return Object.fromEntries(Object.entries(state || {}).filter(([, v]) => v.pausedUntil > now).map(([p, v]) => [p, v.pausedUntil]));
}

/** A feedback event for the agent (POST /api/v1/apply/events). */
export function eventFor(cur, kind, extra = {}) {
  const link = cur?.link || {};
  return {
    kind,
    link_id: link.id ?? null,
    url: link.url ?? cur?.url ?? null,
    platform: link.platform ?? null,
    person: cur?.person ?? null,
    field: null,
    reason: null,
    detail: null,
    ...extra,
  };
}
