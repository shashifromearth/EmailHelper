// In-page field discovery. `discoverFields` is serialised and executed inside
// the page with frame.evaluate(), so it must be fully self-contained.
//
// It finds the application form, enumerates its fields, resolves labels, and
// marks every control with data-eh-field="<id>" (and data-eh-opt="<index>" for
// radio/checkbox group members) so the Node side can address them later.

export function discoverFields() {
  const MARK = 'data-eh-field';
  const OPT = 'data-eh-opt';
  for (const el of document.querySelectorAll('[data-eh-field],[data-eh-apply],[data-eh-submit]')) {
    el.removeAttribute(MARK);
    el.removeAttribute(OPT);
    el.removeAttribute('data-eh-apply');
    el.removeAttribute('data-eh-submit');
  }

  const norm = (s) => (s || '').replace(/[\s\u00a0\u200b]+/g, ' ').trim();
  const REQ_RE = /[*\u2731\u2217\uff0a]|\(required\)/i;
  const visible = (el) => {
    if (!el || !el.isConnected) return false;
    if (typeof el.checkVisibility === 'function') {
      return el.checkVisibility({ visibilityProperty: true, checkVisibilityCSS: true });
    }
    const s = getComputedStyle(el);
    return s.display !== 'none' && s.visibility !== 'hidden' && el.getClientRects().length > 0;
  };
  const textOf = (node) => {
    if (!node) return '';
    const c = node.cloneNode(true);
    c.querySelectorAll('input,select,textarea,button,script,style,noscript,svg,option,template').forEach((x) => x.remove());
    return norm(c.textContent);
  };
  const cleanLabel = (s) =>
    norm(String(s || '').replace(/\(required\)/gi, '').replace(/[*\u2731\u2217\uff0a]/g, ''))
      .replace(/\s*:\s*$/, '')
      .slice(0, 300);

  const kindOf = (el) => {
    if (el.tagName === 'SELECT') return 'select';
    if (el.tagName === 'TEXTAREA') return 'textarea';
    const t = (el.getAttribute('type') || 'text').toLowerCase();
    if (['text', 'email', 'tel', 'url', 'number', 'date', 'radio', 'checkbox', 'file'].includes(t)) return t;
    if (['month', 'week', 'time', 'datetime-local'].includes(t)) return 'text';
    return null; // hidden, submit, button, reset, image, password, search, color, range, ...
  };

  const usable = (el, kind) => {
    if (el.disabled) return false;
    if (kind === 'file') return true; // ATS file inputs are usually visually hidden behind a button
    if (['select', 'radio', 'checkbox'].includes(kind)) {
      // custom widgets hide the native control but keep its wrapper visible
      return visible(el) || visible(el.parentElement);
    }
    if (el.readOnly) return false;
    if (el.closest('[aria-hidden="true"]')) return false; // honeypots
    return visible(el);
  };

  // ---- label resolution -------------------------------------------------
  const LABELISH = 'label,legend,[class*="label" i],[class*="question" i],[class*="title" i],h1,h2,h3,h4,h5,h6';
  const hasControl = (n) => n.matches('input,select,textarea') || !!n.querySelector('input:not([type="hidden"]),select,textarea');

  const fromLabels = (el) => {
    const labels = el.labels ? [...el.labels] : [];
    for (const lb of labels) {
      if (lb.contains(el)) {
        // wrapping label (Lever style): prefer an inner label-ish element that doesn't hold the control
        const inner = [...lb.querySelectorAll('[class*="label" i]')].find((x) => !x.contains(el) && textOf(x));
        if (inner) return textOf(inner);
      }
      const t = textOf(lb);
      if (t) return t;
    }
    return '';
  };
  const fromLabelledBy = (el) => {
    const ids = (el.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
    return norm(ids.map((id) => textOf(document.getElementById(id))).join(' '));
  };
  const precedingText = (start, stop) => {
    let node = start;
    for (let depth = 0; node && node !== stop && node !== document.body && depth < 7; depth++) {
      if (node.tagName === 'FIELDSET') {
        const lg = node.querySelector(':scope > legend');
        if (lg && textOf(lg)) return lg.textContent;
      }
      let sib = node.previousElementSibling;
      let hops = 0;
      let firstText = '';
      while (sib && hops < 4) {
        if (sib.matches('script,style,template,br,hr')) {
          sib = sib.previousElementSibling;
          continue;
        }
        if (hasControl(sib)) break; // belongs to another question
        if (sib.tagName === 'LABEL' && sib.htmlFor && sib.htmlFor !== start.id) break; // label of another control
        const t = textOf(sib);
        if (t && t.length <= 300) {
          if (sib.matches(LABELISH) || sib.querySelector(LABELISH)) return sib.textContent;
          if (!firstText) firstText = sib.textContent;
        }
        sib = sib.previousElementSibling;
        hops++;
      }
      if (firstText) return firstText;
      node = node.parentElement;
    }
    return '';
  };
  const singleLabel = (el, container) =>
    fromLabels(el) ||
    el.getAttribute('aria-label') ||
    fromLabelledBy(el) ||
    precedingText(el, container) ||
    el.getAttribute('placeholder') ||
    el.getAttribute('name') ||
    '';
  const optionLabel = (el) => {
    let t = norm(fromLabels(el)) || norm(el.getAttribute('aria-label')) || fromLabelledBy(el);
    if (!t) {
      let n = el.nextSibling;
      while (n && !t) {
        if (n.nodeType === 3) t = norm(n.textContent);
        else if (n.nodeType === 1) {
          if (hasControl(n)) break;
          t = textOf(n);
        }
        n = n.nextSibling;
      }
    }
    return cleanLabel(t || el.value || '');
  };
  const groupLabel = (members, container) => {
    const fs = members[0].closest('fieldset');
    if (fs && members.every((m) => fs.contains(m))) {
      const lg = fs.querySelector(':scope > legend');
      if (lg && textOf(lg)) return lg.textContent;
    }
    const grp = members[0].closest('[role="radiogroup"],[role="group"]');
    if (grp && members.every((m) => grp.contains(m))) {
      const t = grp.getAttribute('aria-label') || fromLabelledBy(grp);
      if (t) return t;
    }
    return precedingText(members[0], container) || members[0].getAttribute('name') || '';
  };

  // ---- form discovery ---------------------------------------------------
  const collect = (container) => {
    const controls = [...container.querySelectorAll('input,select,textarea')]
      .map((el) => ({ el, kind: kindOf(el) }))
      .filter((c) => c.kind && usable(c.el, c.kind));
    const groups = new Map();
    for (const c of controls) {
      if ((c.kind === 'radio' || c.kind === 'checkbox') && c.el.name) {
        const key = c.kind + '\u0000' + c.el.name;
        if (!groups.has(key)) groups.set(key, []);
        groups.get(key).push(c.el);
      }
    }
    const fields = [];
    const used = new Set();
    const seenGroups = new Set();
    const assignId = (base) => {
      let id = base || `field_${fields.length}`;
      if (used.has(id)) {
        let i = 2;
        while (used.has(`${id}_${i}`)) i++;
        id = `${id}_${i}`;
      }
      used.add(id);
      return id;
    };
    for (const c of controls) {
      const el = c.el;
      const groupKey = c.el.name ? c.kind + '\u0000' + c.el.name : null;
      const members = groupKey ? groups.get(groupKey) : null;
      const isGroup = (c.kind === 'radio' && members) || (c.kind === 'checkbox' && members && members.length > 1);
      if (isGroup) {
        if (seenGroups.has(groupKey)) continue;
        seenGroups.add(groupKey);
        const raw = groupLabel(members, container);
        const id = assignId(el.name);
        const opts = members.map((m, i) => ({ label: optionLabel(m), value: m.value, index: i }));
        members.forEach((m, i) => {
          m.setAttribute(MARK, id);
          m.setAttribute(OPT, String(i));
        });
        const grp = members[0].closest('[role="radiogroup"],[role="group"]');
        fields.push({
          id,
          label: cleanLabel(raw) || el.name,
          type: c.kind,
          required:
            members.some((m) => m.required || m.getAttribute('aria-required') === 'true') ||
            (grp && grp.getAttribute('aria-required') === 'true') ||
            REQ_RE.test(raw || ''),
          options: opts.map((o) => o.label),
          _group: true,
          _multiple: c.kind === 'checkbox',
          _opts: opts,
        });
        continue;
      }
      const raw = singleLabel(el, container);
      const id = assignId(el.id || el.name);
      el.setAttribute(MARK, id);
      const field = {
        id,
        label: cleanLabel(raw) || id,
        type: c.kind,
        required: el.required || el.getAttribute('aria-required') === 'true' || REQ_RE.test(raw || ''),
        options: [],
        _group: false,
        _multiple: false,
        _opts: [],
      };
      if (c.kind === 'select') {
        field._multiple = !!el.multiple;
        field._opts = [...el.options]
          .map((o, index) => ({ label: norm(o.label || o.textContent), value: o.value, index, disabled: o.disabled }))
          .filter((o) => o.value !== '' && !(o.disabled && o.index === 0));
        field.options = field._opts.map((o) => o.label);
      }
      if (c.kind === 'file') {
        const accept = el.getAttribute('accept');
        if (accept) field.accept = accept;
        field._multiple = !!el.multiple;
      }
      fields.push(field);
    }
    return fields;
  };

  const APP_HINT = /e-?mail|phone|mobile|first.?name|last.?name|full.?name|resume|cv\b|cover.?letter|linkedin/i;
  const appLikeGeneric = (fields) =>
    fields.length >= 2 &&
    fields.some((f) => ['email', 'tel', 'file'].includes(f.type) || APP_HINT.test(f.label) || APP_HINT.test(f.id));

  const ATS_SELECTORS = [
    ['greenhouse', '#application_form, form#application-form, #main_fields'],
    ['lever', 'form.application-form, .application-form, form[action*="apply"]'],
    ['ashby', '[class*="ashby-application-form"] form, form[class*="ashby-application-form"], [class*="ashby-application-form"]'],
  ];

  let chosen = null;
  for (const [kind, sel] of ATS_SELECTORS) {
    for (const node of document.querySelectorAll(sel)) {
      const container = node.closest('form') || node;
      const fields = collect(container);
      if (fields.length > 0) {
        chosen = { container, kind, fields, appLike: true };
        break;
      }
    }
    if (chosen) break;
  }
  if (!chosen) {
    let best = null;
    for (const form of document.querySelectorAll('form')) {
      const fields = collect(form);
      if (!best || fields.length > best.fields.length) best = { container: form, kind: 'generic', fields };
    }
    if (!best || best.fields.length === 0) {
      const fields = collect(document.body);
      if (fields.length >= 2) best = { container: document.body, kind: 'body', fields };
    }
    if (best) {
      best.appLike = appLikeGeneric(best.fields);
      chosen = best;
    }
  }
  // collect() marks every visited container; re-mark only the chosen one
  for (const el of document.querySelectorAll('[data-eh-field]')) {
    if (!chosen || !chosen.container.contains(el)) {
      el.removeAttribute(MARK);
      el.removeAttribute(OPT);
    }
  }
  if (chosen && chosen.appLike) {
    // collect() on other forms may have overwritten marks in the chosen one; redo it
    chosen.fields = collect(chosen.container);
  }

  // ---- "Apply" control ----------------------------------------------------
  let apply = null;
  if (!chosen || !chosen.appLike) {
    const APPLY_RE = /^\s*(apply|apply now|apply for this (job|position|role)|apply to this (job|position|role)|apply online|start (your )?application|i'?m interested)\s*[\u203a\u00bb\u2192>]?\s*$/i;
    const cands = document.querySelectorAll('a, button, [role="button"], input[type="button"], input[type="submit"]');
    for (const el of cands) {
      const t = norm(el.tagName === 'INPUT' ? el.value : el.innerText || el.textContent) || norm(el.getAttribute('aria-label'));
      if (t && APPLY_RE.test(t) && visible(el)) {
        el.setAttribute('data-eh-apply', '1');
        const href = el.tagName === 'A' ? el.href : '';
        apply = { text: t, href: /^https?:/i.test(href) && !/#$/.test(el.getAttribute('href') || '') ? href : '' };
        break;
      }
    }
  }

  const markers = {
    greenhouse: !!document.querySelector('#application_form, form#application-form, #main_fields, #grnhse_app, [id^="grnhse"]'),
    lever: !!document.querySelector('form.application-form, .application-form, [data-qa="btn-submit"]'),
    ashby: !!document.querySelector('[class*="ashby-application-form"], [class*="ashby-job-posting"]'),
    workday: !!document.querySelector('[data-automation-id="jobPostingHeader"], [data-automation-id="applyManually"], [data-automation-id]'),
    linkedin: !!document.querySelector('.jobs-easy-apply-modal, .jobs-apply-button'),
  };

  const appLike = !!(chosen && chosen.appLike);
  return {
    appLike,
    containerKind: chosen ? chosen.kind : null,
    fields: appLike ? chosen.fields : [],
    apply,
    markers,
    controlCount: document.querySelectorAll('input:not([type="hidden"]),select,textarea').length,
  };
}

/** In-page: read current values of marked fields to compute missing_required. */
export function readFieldStates(ids) {
  const q = (id) => document.querySelectorAll('[data-eh-field="' + id.replace(/["\\]/g, '\\$&') + '"]');
  const out = {};
  for (const id of ids) {
    const els = [...q(id)];
    if (els.length === 0) {
      out[id] = { present: false, empty: true };
      continue;
    }
    const el = els[0];
    let empty;
    if (el.tagName === 'SELECT') {
      empty = ![...el.selectedOptions].some((o) => o.value !== '');
    } else if (el.type === 'radio' || el.type === 'checkbox') {
      empty = !els.some((e) => e.checked);
    } else if (el.type === 'file') {
      empty = !(el.files && el.files.length > 0);
    } else {
      empty = !String(el.value || '').trim();
    }
    out[id] = { present: true, empty };
  }
  return out;
}

/** In-page: find and mark the submit button inside the marked form. */
export function markSubmitButton() {
  document.querySelectorAll('[data-eh-submit]').forEach((e) => e.removeAttribute('data-eh-submit'));
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => (el.checkVisibility ? el.checkVisibility({ visibilityProperty: true }) : el.getClientRects().length > 0);
  const anyField = document.querySelector('[data-eh-field]');
  const scopes = [];
  if (anyField) {
    const form = anyField.closest('form');
    if (form) scopes.push(form);
  }
  scopes.push(document);
  const TEXT_RE = /^(submit|submit application|submit your application|send application|apply|apply now)$/i;
  for (const scope of scopes) {
    const cands = [
      ...scope.querySelectorAll('button[type="submit"], input[type="submit"], [data-qa="btn-submit"], button#btn-submit'),
      ...[...scope.querySelectorAll('button, [role="button"]')].filter((b) => TEXT_RE.test(norm(b.innerText || b.textContent))),
    ];
    const btn = cands.find((b) => visible(b) && !b.disabled);
    if (btn) {
      btn.setAttribute('data-eh-submit', '1');
      return { text: norm(btn.tagName === 'INPUT' ? btn.value : btn.innerText || btn.textContent) };
    }
  }
  return null;
}
