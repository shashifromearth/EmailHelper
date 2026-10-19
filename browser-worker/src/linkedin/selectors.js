// All LinkedIn people-search selectors live here so they are easy to update
// when LinkedIn changes its markup. Every entry is a list of fallbacks tried in order.
export const LINKEDIN_SELECTORS = {
  // One element per search result. The first selector that matches anything wins.
  resultCard: [
    'li.reusable-search__result-container',
    'div[data-chameleon-result-urn]',
    'div[data-view-name="search-entity-result-universal-template"]',
    'div.entity-result',
    'div[data-view-name="people-search-result"]',
    'main div[role="listitem"]',
    'main ul[role="list"] > li',
  ],
  // Inside a card.
  name: [
    '.entity-result__title-text a span[aria-hidden="true"]',
    '.entity-result__title-text a',
    'a[data-test-app-aware-link] span[aria-hidden="true"]',
    'a[href*="/in/"] span[dir="ltr"] > span[aria-hidden="true"]',
    'a[href*="/in/"] span[aria-hidden="true"]',
    'a[href*="/in/"]',
  ],
  headline: [
    '.entity-result__primary-subtitle',
    'div.t-14.t-black.t-normal',
    '[data-view-name="search-result-headline"]',
  ],
  location: ['.entity-result__secondary-subtitle', 'div.t-14.t-normal:not(.t-black)'],
  summary: ['.entity-result__summary', 'p.entity-result__summary--2-lines', '[data-view-name="search-result-summary"]'],
  profileLink: ['a[href*="/in/"]'],
  // Page-level markers.
  noResults: ['.search-reusable-search-no-results', '.artdeco-empty-state', '[data-view-name="search-no-results"]'],
  resultsReady: ['.search-results-container', 'main [data-view-name="search-results"]', 'main'],
};

export const LINKEDIN_PEOPLE_SEARCH_PATH = '/search/results/people/';
