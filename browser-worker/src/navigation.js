// Navigation, settling, frame-aware field discovery and the "click Apply" flow.
import { discoverFields } from './dom/discover.js';
import { setMainStatus } from './blockers.js';
import { pollUntil } from './sync.js';

export function isHttpUrl(u) {
  try {
    const p = new URL(u);
    return p.protocol === 'http:' || p.protocol === 'https:';
  } catch {
    return false;
  }
}

/** Current page URL, or `fallback` when the page shows a browser error page / about:blank. */
export function pageUrl(page, fallback) {
  const u = page.url();
  return isHttpUrl(u) ? u : fallback;
}

export async function navigate(page, url, cfg) {
  try {
    const resp = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: cfg.navTimeoutMs });
    if (resp) setMainStatus(page, resp.status());
    return { status: resp ? resp.status() : null, error: null };
  } catch (e) {
    return { status: null, error: e.message.split('\n')[0] };
  }
}

/** Short network-idle wait, capped so long-polling pages don't stall us. */
export async function settle(page, cfg) {
  if (cfg.networkIdleCapMs > 0) {
    await page.waitForLoadState('networkidle', { timeout: cfg.networkIdleCapMs }).catch(() => {});
  }
}

/**
 * Run discovery in every frame (embedded ATS iframes, e.g. Greenhouse's
 * #grnhse_iframe) and keep the best result.
 */
export async function discoverAll(page) {
  let best = null;
  for (const frame of page.frames()) {
    let r;
    try {
      r = await frame.evaluate(discoverFields);
    } catch {
      continue;
    }
    const score = (x) => (x.appLike ? 100000 : 0) + x.fields.length;
    if (!best || score(r) > score(best) || (frame === page.mainFrame() && score(r) === score(best))) {
      best = { ...r, frame };
    }
  }
  return best || { appLike: false, containerKind: null, fields: [], apply: null, markers: {}, frame: page.mainFrame() };
}

export function detectAts(url, markers = {}) {
  let host = '';
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {}
  if (/(^|\.)greenhouse\.io$/.test(host)) return 'greenhouse';
  if (/(^|\.)lever\.co$/.test(host)) return 'lever';
  if (/(^|\.)ashbyhq\.com$/.test(host)) return 'ashby';
  if (/(^|\.)(myworkdayjobs\.com|myworkdaysite\.com|workday\.com)$/.test(host)) return 'workday';
  if (/(^|\.)linkedin\.com$/.test(host)) return 'linkedin';
  if (markers.greenhouse) return 'greenhouse';
  if (markers.lever) return 'lever';
  if (markers.ashby) return 'ashby';
  if (markers.workday) return 'workday';
  if (markers.linkedin) return 'linkedin';
  return 'generic';
}

/**
 * Open `url` in the session page, find the application form and, if there is
 * only an Apply button/link, follow it.
 * @returns {Promise<{navError: string|null, discovery: object}>}
 */
export async function openApplication(mgr, entry, url, log) {
  const cfg = mgr.cfg;
  const nav = await navigate(entry.page, url, cfg);
  entry.url = url;
  if (nav.error) return { navError: nav.error, discovery: null };
  await settle(entry.page, cfg);

  let d = await discoverAll(entry.page);
  if (!d.appLike && !d.apply && !d.controlCount) {
    // client-rendered forms (SPAs) may show up a little later
    d = (await pollUntil(async () => {
      const x = await discoverAll(entry.page);
      return x.appLike || x.apply ? x : null;
    }, { timeoutMs: 3000, intervalMs: 500 })) || d;
  }
  if (!d.appLike && d.apply) {
    log.info('following apply control', { session_key: entry.key, text: d.apply.text });
    if (d.apply.href) {
      const n2 = await navigate(entry.page, d.apply.href, cfg);
      if (n2.error) return { navError: n2.error, discovery: null };
    } else {
      const popupP = entry.page.context().waitForEvent('page', { timeout: 3000 }).catch(() => null);
      try {
        await d.frame.locator('[data-eh-apply]').first().click({ timeout: 5000 });
      } catch (e) {
        log.warn('apply click failed', { error: e.message.split('\n')[0] });
      }
      const popup = await popupP;
      if (popup) {
        mgr.adoptPage(entry, popup);
        await popup.waitForLoadState('domcontentloaded').catch(() => {});
      }
      await entry.page.waitForLoadState('domcontentloaded').catch(() => {});
    }
    await settle(entry.page, cfg);
    d = (await pollUntil(async () => {
      const x = await discoverAll(entry.page);
      return x.appLike ? x : null;
    }, { timeoutMs: 8000, intervalMs: 500 })) || (await discoverAll(entry.page));
  }
  return { navError: null, discovery: d };
}

/** Strip internal (underscore) keys from discovered fields to get contract Field objects. */
export function publicField(f) {
  const out = { id: f.id, label: f.label, type: f.type, required: !!f.required, options: f.options || [] };
  if (f.accept) out.accept = f.accept;
  return out;
}
