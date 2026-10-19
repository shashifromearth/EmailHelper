// Blocker detection shared by extract / inspect / fill / linkedin.
import { probeBlockers } from './dom/probe.js';

export const LOGIN_URL_RE = /login|signin|sign-in|sign_in|authwall|checkpoint/i;

// Per-page status of the last main-frame navigation response (incl. form posts / clicks).
const mainStatus = new WeakMap();

export function trackMainStatus(page) {
  page.on('response', (resp) => {
    try {
      const req = resp.request();
      if (req.isNavigationRequest() && req.frame() === page.mainFrame()) mainStatus.set(page, resp.status());
    } catch {
      /* frame detached */
    }
  });
}

export function getMainStatus(page) {
  return mainStatus.get(page) ?? null;
}

export function setMainStatus(page, status) {
  if (status != null) mainStatus.set(page, status);
}

function urlForMatch(url) {
  try {
    const u = new URL(url);
    return u.hostname + u.pathname;
  } catch {
    return String(url || '');
  }
}

/** Run the in-page probe in the main frame (and optionally one more frame) and merge. */
export async function probe(page, extraFrame = null) {
  const frames = [page.mainFrame()];
  if (extraFrame && extraFrame !== page.mainFrame()) frames.push(extraFrame);
  let merged = null;
  for (const f of frames) {
    let r;
    try {
      r = await f.evaluate(probeBlockers);
    } catch {
      continue;
    }
    if (!merged) merged = r;
    else {
      merged.captcha ||= r.captcha;
      merged.cloudflare ||= r.cloudflare;
      merged.otp ||= r.otp;
      merged.mfa ||= r.mfa;
      merged.passwordVisible ||= r.passwordVisible;
    }
  }
  return merged;
}

/**
 * Pure decision function: signals -> Blocker | null.
 * @param {object} s
 * @param {string|null} s.navError  navigation error message
 * @param {number|null} s.status    last main-frame HTTP status
 * @param {string} s.url            current page URL
 * @param {object|null} s.probe     result of probeBlockers()
 * @param {Array|null} s.fields     discovered application fields (null when not inspected)
 */
export function decideBlocker({ navError = null, status = null, url = '', probe: p = null, fields = null }) {
  if (navError) return { kind: 'network_error', detail: String(navError).split('\n')[0].slice(0, 300) };
  if (p && p.cloudflare) return { kind: 'captcha', detail: 'Cloudflare browser challenge page' };
  if (status === 429 || (p && p.tooManyText)) {
    return { kind: 'rate_limited', detail: status === 429 ? 'HTTP 429 Too Many Requests' : 'page says "too many requests"' };
  }
  if (status === 999) return { kind: 'rate_limited', detail: 'HTTP 999 (request denied by LinkedIn anti-automation)' };
  if (status === 401 || status === 403 || (p && p.accessDeniedText)) {
    return { kind: 'access_denied', detail: status === 401 || status === 403 ? `HTTP ${status}` : 'page says "access denied"' };
  }
  if (status != null && status >= 500) return { kind: 'site_error', detail: `HTTP ${status}` };
  if (p && p.captcha) return { kind: 'captcha', detail: p.captcha };
  if (p && p.mfa) return { kind: 'mfa', detail: p.mfa };
  if (p && p.otp) return { kind: 'otp', detail: p.otp };
  if (LOGIN_URL_RE.test(urlForMatch(url))) return { kind: 'login_required', detail: `redirected to a login page: ${urlForMatch(url).slice(0, 200)}` };
  if (p && p.passwordVisible) {
    const noAppFields = fields
      ? fields.length <= 2 && !fields.some((f) => f.type === 'file' || f.type === 'textarea')
      : p.otherInputs <= 2;
    if (noAppFields) return { kind: 'login_required', detail: 'password field present and no application fields' };
  }
  return null;
}

export async function detectBlocker(page, { navError = null, fields = null, frame = null } = {}) {
  if (navError) return decideBlocker({ navError });
  const p = await probe(page, frame);
  return decideBlocker({ status: getMainStatus(page), url: page.url(), probe: p, fields });
}
