// LinkedIn people-search parser tests (fixture page) + end-to-end against a mock.
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { chromium } from 'playwright';
import { extractRawCards, parsePeople, normalizeProfileUrl, cleanName, companyMatcher } from '../src/linkedin/parser.js';
import { LINKEDIN_SELECTORS } from '../src/linkedin/selectors.js';
import { MOCK_DIR, startMock, startWorker } from './helpers.js';

const FIXTURE = path.join(MOCK_DIR, 'linkedin', 'search.html');

describe('linkedin parser (pure)', () => {
  it('normalizes profile URLs', () => {
    assert.equal(normalizeProfileUrl('https://www.linkedin.com/in/jane-doe-123/?miniProfileUrn=x'), 'https://www.linkedin.com/in/jane-doe-123');
    assert.equal(normalizeProfileUrl('/in/jane/'), 'https://www.linkedin.com/in/jane');
    assert.equal(normalizeProfileUrl('https://in.linkedin.com/in/jane?trk=a'), 'https://www.linkedin.com/in/jane');
    assert.equal(normalizeProfileUrl('https://www.linkedin.com/search/results/people/'), null);
    assert.equal(normalizeProfileUrl('https://evil.example/in/jane'), null);
    assert.equal(normalizeProfileUrl(''), null);
  });
  it('cleans names', () => {
    assert.equal(cleanName('Priya Sharma View Priya Sharma’s profile'), 'Priya Sharma');
    assert.equal(cleanName('  Rahul   Verma • 2nd '), 'Rahul Verma');
  });
  it('matches company names on word boundaries', () => {
    const m = companyMatcher('Acme Inc.');
    assert.ok(m('Recruiter at Acme'));
    assert.ok(m('Hiring for ACME Engineering'));
    assert.ok(!m('Recruiter at AcmeSoft Labs'));
  });
  it('dedupes, filters by company and caps', () => {
    const raw = [
      { name: 'A', headline: 'Recruiter at Acme', hrefs: ['/in/a/'] },
      { name: 'A again', headline: 'Recruiter at Acme', hrefs: ['https://www.linkedin.com/in/A?x=1'] },
      { name: 'B', headline: 'Recruiter at Globex', hrefs: ['/in/b'] },
      { name: 'LinkedIn Member', headline: 'Recruiter at Acme', hrefs: ['/search/results/people/'] },
      { name: 'C', headline: 'People', summary: 'Current: HR at Acme', hrefs: ['/in/c'] },
      { name: 'D', headline: 'TA at Acme', hrefs: ['/in/d'] },
    ];
    assert.deepEqual(parsePeople(raw, { company: 'Acme', limit: 10 }).map((p) => p.name), ['A', 'C', 'D']);
    assert.deepEqual(parsePeople(raw, { company: 'Acme', limit: 2 }).map((p) => p.name), ['A', 'C']);
    assert.equal(parsePeople(raw, { company: 'Acme', filterCompany: false, limit: 10 }).length, 4);
  });
});

describe('linkedin parser on the fixture page', () => {
  let browser;
  before(async () => {
    browser = await chromium.launch({ headless: true });
  });
  after(async () => {
    await browser?.close();
  });

  it('extracts cards and parses people', async () => {
    const page = await browser.newPage();
    await page.setContent(await fs.readFile(FIXTURE, 'utf8'));
    const raw = await page.evaluate(extractRawCards, LINKEDIN_SELECTORS);
    assert.equal(raw.length, 7); // the "LinkedIn Member" card has no /in/ link and is dropped
    assert.equal(raw[0].name, 'Priya Sharma');
    assert.equal(raw[0].headline, 'Senior Technical Recruiter at Acme');
    const people = parsePeople(raw, { company: 'Acme', limit: 10 });
    assert.deepEqual(people, [
      { name: 'Priya Sharma', headline: 'Senior Technical Recruiter at Acme', profile_url: 'https://www.linkedin.com/in/priya-sharma-recruiter' },
      { name: 'Rahul Verma', headline: 'Talent Acquisition Lead | Hiring for ACME Engineering', profile_url: 'https://www.linkedin.com/in/rahul-verma-ta' },
      { name: 'Anita Desai', headline: 'People Partner', profile_url: 'https://www.linkedin.com/in/anita-desai-hr' },
      { name: 'Meera Iyer', headline: 'Engineering Manager, Hiring Manager for AI @ Acme', profile_url: 'https://www.linkedin.com/in/meera-iyer-acme' },
    ]);
    await page.close();
  });
});

describe('POST /v1/linkedin/people (mock LinkedIn)', () => {
  let mock;
  before(async () => {
    mock = await startMock();
  });
  after(async () => {
    await mock?.close();
  });

  it('reads results; stops searching once the limit is reached', async () => {
    const w = await startWorker({ LINKEDIN_BASE_URL: `${mock.url}/li` });
    try {
      const r = await w.call('POST', '/v1/linkedin/people', { company: 'Acme', roles: ['recruiter', 'talent acquisition'], limit: 3 });
      assert.equal(r.status, 200, r.text);
      assert.equal(r.body.blocker, null);
      assert.deepEqual(r.body.people.map((p) => p.name), ['Priya Sharma', 'Rahul Verma', 'Anita Desai']);
      assert.ok(r.body.people.every((p) => p.profile_url.startsWith('https://www.linkedin.com/in/')));
      const hits = (await (await fetch(`${mock.url}/__submissions`)).json()).linkedin_hits;
      assert.deepEqual(hits.map((h) => h.keywords), ['recruiter Acme']);
    } finally {
      await w.close();
    }
  });

  it('authwall redirect -> login_required', async () => {
    const w = await startWorker({ LINKEDIN_BASE_URL: `${mock.url}/li` });
    try {
      const r = await w.call('POST', '/v1/linkedin/people', { company: 'AuthwallCorp', roles: ['recruiter'], limit: 5 });
      assert.equal(r.status, 200, r.text);
      assert.deepEqual(r.body.people, []);
      assert.equal(r.body.blocker.kind, 'login_required');
    } finally {
      await w.close();
    }
  });

  it('validates the request', async () => {
    const w = await startWorker({ LINKEDIN_BASE_URL: `${mock.url}/li` });
    try {
      assert.equal((await w.call('POST', '/v1/linkedin/people', { roles: ['x'] })).status, 400);
      assert.equal((await w.call('POST', '/v1/linkedin/people', { company: 'A', roles: 'x' })).status, 400);
      assert.equal((await w.call('POST', '/v1/linkedin/people', { company: 'A', limit: 0 })).status, 400);
    } finally {
      await w.close();
    }
  });
});
