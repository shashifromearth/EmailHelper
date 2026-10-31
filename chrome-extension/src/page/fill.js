// In-page actions, serialised with chrome.scripting.executeScript(): each
// function must be self-contained. Fields are addressed by the
// data-eh-field / data-eh-opt marks that discoverFields() left.

/** Apply fill steps (see buildPlan). Returns { filled: [ids], failed: [{id, reason}] }. */
export async function fillFields(steps) {
  const sel = (id) => '[data-eh-field="' + String(id).replace(/["\\]/g, '\\$&') + '"]';
  const all = (id) => [...document.querySelectorAll(sel(id))];
  const opt = (id, i) => document.querySelector(sel(id) + '[data-eh-opt="' + i + '"]');
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const fire = (el, ...types) => types.forEach((t) => el.dispatchEvent(new Event(t, { bubbles: true })));
  // React and similar keep their own copy of the value: go through the native setter.
  const setValue = (el, text) => {
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    el.focus();
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, text);
    fire(el, 'input', 'change');
  };
  const setChecked = (el, want) => {
    if (el.checked !== want) el.click();
    if (el.checked !== want) {
      el.checked = want;
      fire(el, 'input', 'change');
    }
  };
  const words = (s) => String(s).toLowerCase().split(/[^\p{L}\p{N}]+/u).filter((w) => w.length >= 3);
  const pickOption = (el, want) => {
    const lbId = el.getAttribute('aria-controls') || el.getAttribute('aria-owns');
    const lb = lbId ? document.getElementById(lbId) : null;
    const opts = [...(lb || document).querySelectorAll('[role="option"]')].filter((o) => o.getClientRects().length > 0);
    const text = (o) => (o.innerText || o.textContent || '').replace(/\s+/g, ' ').trim().toLowerCase();
    const w = want.toLowerCase();
    const ws = words(want);
    return (
      opts.find((o) => text(o) === w) ||
      opts.filter((o) => ws.length && ws.every((x) => text(o).includes(x))).sort((a, b) => text(a).length - text(b).length)[0]
    );
  };
  const combo = async (el, want) => {
    for (const attempt of [want, want.split(',')[0]]) {
      el.click();
      setValue(el, attempt.slice(0, 40));
      for (let i = 0; i < 15; i++) {
        await sleep(200);
        const o = pickOption(el, want);
        if (o) {
          o.click();
          return null;
        }
      }
    }
    el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    return `no option matching "${want}"`;
  };

  const filled = [];
  const failed = [];
  for (const s of steps) {
    const el = all(s.id)[0];
    if (!el) {
      failed.push({ id: s.id, reason: 'field is gone' });
      continue;
    }
    try {
      let problem = null;
      if (s.type === 'select') {
        const want = new Set(s.indexes);
        [...el.options].forEach((o, i) => (o.selected = want.has(i)));
        fire(el, 'input', 'change');
      } else if (s.type === 'radio') {
        const target = opt(s.id, s.indexes[0]);
        if (!target) problem = 'option is gone';
        else if (s.buttons) {
          if (target.getAttribute('aria-pressed') !== 'true') target.click();
        } else setChecked(target, true);
      } else if (s.type === 'checkbox') {
        if (s.indexes) {
          const want = new Set(s.indexes);
          for (const box of all(s.id)) setChecked(box, want.has(Number(box.getAttribute('data-eh-opt'))));
        } else setChecked(el, s.checked);
      } else if (s.type === 'combobox') {
        problem = await combo(el, s.text);
      } else {
        setValue(el, s.text);
        fire(el, 'blur');
      }
      if (problem) failed.push({ id: s.id, reason: problem });
      else filled.push(s.id);
    } catch (e) {
      failed.push({ id: s.id, reason: String(e && e.message ? e.message : e).slice(0, 200) });
    }
  }
  return { filled, failed };
}

/** Put a file (base64) into a marked file input. Returns null or a reason. */
export function attachFile(id, name, type, base64) {
  const el = document.querySelector('[data-eh-field="' + String(id).replace(/["\\]/g, '\\$&') + '"]');
  if (!el) return 'file field is gone';
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const dt = new DataTransfer();
  dt.items.add(new File([bytes], name, { type }));
  el.files = dt.files;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return el.files.length ? null : 'the page did not accept the file';
}

/** Outline what the person must do: questions left (amber) and the submit button (green). */
export function highlight(asks) {
  document.querySelectorAll('[data-eh-mark]').forEach((e) => {
    e.style.outline = '';
    e.removeAttribute('data-eh-mark');
  });
  const mark = (el, color, title) => {
    el.setAttribute('data-eh-mark', '1');
    el.style.outline = `3px solid ${color}`;
    el.style.outlineOffset = '2px';
    if (title) el.title = title;
  };
  let first = null;
  for (const a of asks) {
    const el = document.querySelector('[data-eh-field="' + String(a.id).replace(/["\\]/g, '\\$&') + '"]');
    if (!el) continue;
    const target = ['radio', 'checkbox'].includes(el.type) ? el.closest('fieldset, [role="group"], [role="radiogroup"]') || el : el;
    mark(target, '#e8a317', 'EmailHelper: please answer this yourself' + (a.reason ? ` (${a.reason})` : ''));
    first ||= target;
  }
  const submit = document.querySelector('[data-eh-submit]');
  if (submit) mark(submit, '#1a9e3f', 'EmailHelper: review the form, then submit');
  (first || submit)?.scrollIntoView({ block: 'center', behavior: 'smooth' });
  return { submit: !!submit };
}

/** Click the job page's "Apply" control that discoverFields() marked. */
export function clickApply() {
  const el = document.querySelector('[data-eh-apply]');
  if (!el) return false;
  el.click();
  return true;
}

/**
 * When the person submits, report what they answered in the questions left
 * for them ({ id: text } or { id: { picked: [option indexes] } }), so the
 * agent can reuse those answers next time.
 */
export function watchSubmit(ids) {
  const read = () => {
    const out = {};
    for (const id of ids) {
      const els = [...document.querySelectorAll('[data-eh-field="' + String(id).replace(/["\\]/g, '\\$&') + '"]')];
      const el = els[0];
      if (!el || el.type === 'file') continue;
      if (el.tagName === 'SELECT') {
        out[id] = [...el.selectedOptions].filter((o) => o.value !== '').map((o) => o.textContent.trim()).join(', ');
      } else if (el.hasAttribute('data-eh-opt')) {
        out[id] = { picked: els.filter((e) => e.checked || e.getAttribute('aria-pressed') === 'true').map((e) => Number(e.getAttribute('data-eh-opt'))) };
      } else if (el.type !== 'checkbox' && el.type !== 'radio') {
        out[id] = String(el.value || '').trim();
      }
    }
    return out;
  };
  const report = () => chrome.runtime.sendMessage({ type: 'answered', values: read() }).catch(() => {});
  const submit = document.querySelector('[data-eh-submit]');
  submit?.addEventListener('click', report, { capture: true });
  (submit?.closest('form') || document).addEventListener('submit', report, { capture: true });
}

/** Click the submit button that markSubmitButton() marked. Returns false when it is gone. */
export function clickSubmit() {
  const el = document.querySelector('[data-eh-submit]');
  if (!el || el.disabled) return false;
  el.click();
  return true;
}

/**
 * What the page shows after a submit: its text (checked for a confirmation by
 * src/plan.js), whether the marked form is gone, visible validation errors,
 * and how many inputs are visible (a new form = the next step).
 */
export function readOutcome() {
  const norm = (s) => String(s || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
  const errors = [];
  for (const el of document.querySelectorAll('[aria-invalid="true"], .error, .field-error')) {
    if (!visible(el)) continue;
    let t = ['INPUT', 'SELECT', 'TEXTAREA'].includes(el.tagName) ? '' : norm(el.innerText || el.textContent);
    if (!t && el.getAttribute('aria-invalid') === 'true') {
      const label = el.labels && el.labels[0];
      t = 'invalid: ' + norm(label ? label.textContent : el.getAttribute('aria-label') || el.name || el.id);
    }
    t = t.slice(0, 200);
    if (t && !errors.includes(t)) errors.push(t);
    if (errors.length >= 10) break;
  }
  const inputs = [
    ...document.querySelectorAll(
      'input:not([type="hidden"]):not([type="submit"]):not([type="button"]):not([type="search"]), select, textarea',
    ),
  ].filter(visible).length;
  return {
    text: norm(document.body ? document.body.innerText : '').slice(0, 20000),
    formGone: !document.querySelector('[data-eh-field]'),
    errors,
    inputs,
  };
}
