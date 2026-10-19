// POST /v1/linkedin/people
//
// Read-only: loads the people-search results page with the user's own logged-in
// session and parses the visible cards. Never opens profiles in bulk, never
// sends messages or connection requests. At most one LinkedIn navigation per
// cfg.linkedinMinIntervalMs (>= 20 s), enforced across all requests.
import { IntervalLimiter, sleep } from '../sync.js';
import { navigate, settle } from '../navigation.js';
import { detectBlocker } from '../blockers.js';
import { LINKEDIN_SELECTORS, LINKEDIN_PEOPLE_SEARCH_PATH } from './selectors.js';
import { extractRawCards, parsePeople } from './parser.js';

export function buildSearchUrl(baseUrl, role, company) {
  const keywords = `${role || ''} ${company || ''}`.replace(/\s+/g, ' ').trim();
  return `${baseUrl}${LINKEDIN_PEOPLE_SEARCH_PATH}?keywords=${encodeURIComponent(keywords)}`;
}

export function createLinkedInPeople(mgr, log) {
  const cfg = mgr.cfg;
  const limiter = new IntervalLimiter(cfg.linkedinMinIntervalMs);

  async function searchOnce(url) {
    await limiter.wait();
    return mgr.withEphemeralPage(async (page) => {
      log.info('linkedin search', { url });
      const nav = await navigate(page, url, cfg);
      if (nav.error) return { blocker: await detectBlocker(page, { navError: nav.error }), raw: [] };
      const sel = LINKEDIN_SELECTORS;
      await page
        .waitForFunction(
          (s) =>
            !location.pathname.startsWith('/search') ||
            [...s.resultCard, ...s.noResults].some((q) => document.querySelector(q)) ||
            !!document.querySelector('input[type="password"]'),
          sel,
          { timeout: 10000 },
        )
        .catch(() => {});
      let blocker = await detectBlocker(page);
      if (!blocker) {
        let path = '';
        try {
          path = new URL(page.url()).pathname;
        } catch {}
        if (!path.includes('/search/')) {
          blocker = { kind: 'login_required', detail: `redirected away from search results to ${path || page.url()}` };
        }
      }
      if (blocker) return { blocker, raw: [] };
      // one gentle scroll so lazily rendered cards on the first page appear
      await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight)).catch(() => {});
      await sleep(800);
      await settle(page, cfg);
      const raw = await page.evaluate(extractRawCards, sel);
      return { blocker: null, raw };
    });
  }

  return async function people({ company, roles, limit }) {
    const out = [];
    const seen = new Set();
    let blocker = null;
    const searchRoles = roles.length ? roles.slice(0, cfg.linkedinMaxSearches) : [''];
    for (const role of searchRoles) {
      if (out.length >= limit) break;
      const r = await searchOnce(buildSearchUrl(cfg.linkedinBaseUrl, role, company));
      if (r.blocker) {
        blocker = r.blocker;
        break;
      }
      for (const p of parsePeople(r.raw, { company, limit: Number.MAX_SAFE_INTEGER })) {
        const k = p.profile_url.toLowerCase();
        if (seen.has(k)) continue;
        seen.add(k);
        out.push(p);
        if (out.length >= limit) break;
      }
    }
    log.info('linkedin search done', { company, results: out.length, blocker: blocker?.kind ?? null });
    return { people: out, blocker };
  };
}
