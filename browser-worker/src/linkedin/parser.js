// LinkedIn people-search parsing.
//
// Two steps so the logic can be unit tested without LinkedIn:
//  1. extractRawCards(selectors)  - runs inside the page (or a fixture loaded
//     with page.setContent) and returns plain "HTML-ish" records per card.
//  2. parsePeople(rawCards, opts) - a pure function that cleans, normalises,
//     de-duplicates, filters by company and caps the list.

/** In-page. Must stay self-contained (serialised by page.evaluate). */
export function extractRawCards(sel) {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const firstText = (root, list) => {
    for (const s of list) {
      const el = root.querySelector(s);
      if (el && norm(el.innerText || el.textContent)) return norm(el.innerText || el.textContent);
    }
    return '';
  };
  let cards = [];
  for (const s of sel.resultCard) {
    const found = [...document.querySelectorAll(s)].filter((c) => c.querySelector(sel.profileLink.join(',')));
    if (found.length) {
      cards = found;
      break;
    }
  }
  if (!cards.length) {
    // last resort: group profile links by their closest list item / container
    const seen = new Set();
    for (const a of document.querySelectorAll(sel.profileLink.join(','))) {
      const c = a.closest('li') || a.parentElement;
      if (c && !seen.has(c)) {
        seen.add(c);
        cards.push(c);
      }
    }
  }
  return cards.map((card) => ({
    name: firstText(card, sel.name),
    headline: firstText(card, sel.headline),
    location: firstText(card, sel.location),
    summary: firstText(card, sel.summary),
    hrefs: [...card.querySelectorAll(sel.profileLink.join(','))].map((a) => a.getAttribute('href') || ''),
  }));
}

const clean = (s) => String(s || '').replace(/\s+/g, ' ').trim();

/** https://www.linkedin.com/in/<slug> or null. Accepts absolute or relative hrefs. */
export function normalizeProfileUrl(href) {
  if (!href) return null;
  let u;
  try {
    u = new URL(href, 'https://www.linkedin.com');
  } catch {
    return null;
  }
  if (!/(^|\.)linkedin\.com$/i.test(u.hostname)) return null;
  const m = u.pathname.match(/^\/in\/([^/?#]+)/i);
  if (!m) return null;
  const slug = m[1];
  if (!slug || slug === 'unavailable') return null;
  return `https://www.linkedin.com/in/${slug}`;
}

export function cleanName(raw) {
  let n = clean(raw);
  n = n.replace(/View .*?[’']s?\s+profile.*$/i, '');
  n = n.replace(/\s*[•·]\s*(1st|2nd|3rd\+?|3rd|out of network).*$/i, '');
  n = n.replace(/\b(Status is (online|offline|reachable|away))\b.*$/i, '');
  n = n.replace(/\s+(1st|2nd|3rd\+?)\s+degree connection.*$/i, '');
  return clean(n);
}

const LEGAL_SUFFIX = /\b(inc|incorporated|llc|ltd|limited|pvt|private|corp|corporation|co|company|gmbh|plc|ag|sa|bv|llp)\b\.?/gi;

export function companyMatcher(company) {
  const base = clean(String(company || '').replace(LEGAL_SUFFIX, ' '))
    .replace(/[.,]+$/, '')
    .toLowerCase();
  if (!base) return () => true;
  const esc = base.replace(/[.*+?^${}()|[\]\\]/g, '\\$&').replace(/\s+/g, '\\s+');
  const re = new RegExp(`(^|[^\\p{L}\\p{N}])${esc}($|[^\\p{L}\\p{N}])`, 'iu');
  return (text) => re.test(String(text || ''));
}

/**
 * @param {Array<{name, headline, location?, summary?, hrefs: string[]}>} rawCards
 * @param {{company?: string, limit?: number, filterCompany?: boolean}} opts
 * @returns {Array<{name: string, headline: string, profile_url: string}>}
 */
export function parsePeople(rawCards, { company = '', limit = 10, filterCompany = true } = {}) {
  const matches = companyMatcher(company);
  const seen = new Set();
  const out = [];
  for (const c of rawCards || []) {
    const profile_url = (c.hrefs || []).map(normalizeProfileUrl).find(Boolean);
    if (!profile_url) continue;
    const name = cleanName(c.name);
    if (!name || /^linkedin member$/i.test(name)) continue;
    const key = profile_url.toLowerCase();
    if (seen.has(key)) continue;
    const headline = clean(c.headline);
    if (filterCompany && company && !(matches(headline) || matches(c.summary))) continue;
    seen.add(key);
    out.push({ name, headline, profile_url });
    if (out.length >= limit) break;
  }
  return out;
}
