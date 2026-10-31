// In-page blocker probe. Serialised into the page with chrome.scripting.executeScript(), so it
// must be self-contained. Returns raw signals; src/plan.js turns them into
// a blocker.

export function probeBlockers() {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => {
    if (!el || !el.isConnected) return false;
    if (typeof el.checkVisibility === 'function') return el.checkVisibility({ visibilityProperty: true, checkVisibilityCSS: true });
    const s = getComputedStyle(el);
    return s.display !== 'none' && s.visibility !== 'hidden';
  };
  const out = {
    title: document.title || '',
    captcha: null,
    cloudflare: false,
    otp: null,
    mfa: null,
    passwordVisible: false,
    otherInputs: 0,
    accessDeniedText: false,
    tooManyText: false,
    textLength: 0,
  };

  // CAPTCHA widgets. Invisible reCAPTCHA (size=invisible / data-size="invisible")
  // and hidden challenge frames are ignored: many ATS forms embed invisible
  // reCAPTCHA v3 that never needs a human.
  const CAP_RE = /recaptcha|hcaptcha|challenges\.cloudflare\.com|turnstile|arkoselabs|funcaptcha/i;
  for (const f of document.querySelectorAll('iframe')) {
    const src = f.getAttribute('src') || '';
    if (CAP_RE.test(src) && !/size=invisible/i.test(src) && visible(f)) {
      let host = src;
      try {
        host = new URL(src, location.href).hostname + new URL(src, location.href).pathname;
      } catch {}
      out.captcha = 'captcha iframe present: ' + host.slice(0, 120);
      break;
    }
  }
  if (!out.captcha) {
    const w = [...document.querySelectorAll('.g-recaptcha, .h-captcha, .cf-turnstile, [data-sitekey]')].find(
      (el) => el.getAttribute('data-size') !== 'invisible' && visible(el),
    );
    if (w) out.captcha = 'captcha widget present (' + (w.className || w.tagName).toString().slice(0, 60) + ')';
  }
  if (
    /^(just a moment|attention required|checking your browser)/i.test(out.title) ||
    document.querySelector('#challenge-form, #cf-challenge-running, #challenge-running')
  ) {
    out.cloudflare = true;
  }

  // OTP / MFA inputs
  const OTP_RE = /\botp\b|one[- ]?time|verification code|security code|2fa|two[- ]factor|multi[- ]factor|\bmfa\b|authenticator/i;
  const MFA_RE = /2fa|two[- ]factor|multi[- ]factor|\bmfa\b|authenticator/i;
  for (const el of document.querySelectorAll('input')) {
    const type = (el.getAttribute('type') || 'text').toLowerCase();
    if (!['text', 'tel', 'number', 'password', ''].includes(type) || !visible(el)) continue;
    const labels = el.labels ? [...el.labels].map((l) => l.textContent).join(' ') : '';
    const hay = norm(
      [labels, el.getAttribute('aria-label'), el.getAttribute('placeholder'), el.getAttribute('name'), el.id].join(' '),
    );
    const isOtc = (el.getAttribute('autocomplete') || '').toLowerCase() === 'one-time-code';
    if (isOtc || OTP_RE.test(hay)) {
      const detail = 'verification code input: ' + (hay || 'autocomplete=one-time-code').slice(0, 100);
      if (MFA_RE.test(hay)) out.mfa = detail;
      else out.otp = detail;
      break;
    }
  }

  for (const el of document.querySelectorAll('input, select, textarea')) {
    const type = (el.getAttribute('type') || 'text').toLowerCase();
    if (!visible(el)) continue;
    if (type === 'password') out.passwordVisible = true;
    else if (!['hidden', 'submit', 'button', 'reset', 'image', 'search', 'checkbox', 'radio'].includes(type)) out.otherInputs++;
  }

  const text = norm(document.body ? document.body.innerText : '');
  out.textLength = text.length;
  // Text heuristics only on short pages (error pages), so a job description
  // that happens to contain "access denied" is not misread.
  const short = text.length < 5000;
  const hay = out.title + ' ' + (short ? text : '');
  out.accessDeniedText = /access denied|403 forbidden|you don'?t have permission to access/i.test(hay);
  out.tooManyText = /too many requests|rate limit(ed| exceeded)/i.test(hay);
  return out;
}
