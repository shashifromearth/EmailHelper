// POST /v1/extract
import { navigate, settle, pageUrl } from './navigation.js';
import { detectBlocker } from './blockers.js';

export const MAX_TEXT = 40000;
export const MAX_LINKS = 200;

/** In-page: collect title, text, raw JSON-LD strings and links. */
function readPage(maxLinks) {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const text = norm(document.body ? document.body.innerText : '');
  const jsonld = [...document.querySelectorAll('script[type="application/ld+json"]')].map((s) => s.textContent || '');
  const links = [];
  const seen = new Set();
  for (const a of document.querySelectorAll('a[href]')) {
    const href = a.href;
    if (!/^https?:/i.test(href) || seen.has(href)) continue;
    seen.add(href);
    links.push({ text: norm(a.innerText || a.textContent || a.getAttribute('aria-label') || a.title).slice(0, 200), href });
    if (links.length >= maxLinks) break;
  }
  return { title: document.title || '', text, jsonld, links };
}

/** Parse JSON-LD blocks; arrays are spread and @graph containers flattened. Invalid blocks are skipped. */
export function parseJsonLd(blocks) {
  const out = [];
  const push = (node) => {
    if (Array.isArray(node)) return node.forEach(push);
    if (!node || typeof node !== 'object') return;
    if (Array.isArray(node['@graph'])) {
      const ctx = node['@context'];
      for (const g of node['@graph']) {
        if (g && typeof g === 'object' && !Array.isArray(g) && ctx && !g['@context']) push({ '@context': ctx, ...g });
        else push(g);
      }
      return;
    }
    out.push(node);
  };
  for (const raw of blocks) {
    const s = String(raw).trim();
    if (!s) continue;
    try {
      push(JSON.parse(s));
    } catch {
      try {
        // common breakage: raw control characters inside strings
        push(JSON.parse(s.replace(/[\u0000-\u001f]+/g, ' ')));
      } catch {
        /* skip invalid block */
      }
    }
  }
  return out;
}

export async function extract(mgr, { url }) {
  return mgr.withEphemeralPage(async (page) => {
    const nav = await navigate(page, url, mgr.cfg);
    if (nav.error) {
      return { final_url: url, title: '', text: '', json_ld: [], links: [], blocker: await detectBlocker(page, { navError: nav.error }) };
    }
    await settle(page, mgr.cfg);
    let data;
    try {
      data = await page.evaluate(readPage, MAX_LINKS);
    } catch {
      // page navigated while reading (client redirect); try once more
      await page.waitForLoadState('domcontentloaded').catch(() => {});
      data = await page.evaluate(readPage, MAX_LINKS);
    }
    const blocker = await detectBlocker(page);
    return {
      final_url: pageUrl(page, url),
      title: data.title,
      text: data.text.slice(0, MAX_TEXT),
      json_ld: parseJsonLd(data.jsonld),
      links: data.links,
      blocker,
    };
  });
}
