// Pure unit tests (no browser).
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { loadConfig, isLoopbackHost, ConfigError } from '../src/config.js';
import { decideBlocker } from '../src/blockers.js';
import { matchKeys, chooseOption, isTruthy, screenshotName, cssString } from '../src/apply.js';
import { parseJsonLd } from '../src/extract.js';
import { resolveUpload } from '../src/uploads.js';
import { IntervalLimiter, Semaphore, KeyedMutex } from '../src/sync.js';
import { detectAts } from '../src/navigation.js';
import { buildSearchUrl } from '../src/linkedin/search.js';
import { FIXTURES_DIR, RESUME, makeTmpDir, rmrf, startMock } from './helpers.js';

describe('config', () => {
  it('requires a token', () => {
    assert.throws(() => loadConfig({}), ConfigError);
  });
  it('defaults match the contract', () => {
    const c = loadConfig({ BROWSER_WORKER_TOKEN: 'x' }, '/work');
    assert.equal(c.host, '127.0.0.1');
    assert.equal(c.port, 7801);
    assert.equal(c.headless, true);
    assert.equal(c.navTimeoutMs, 30000);
    assert.equal(c.allowedUploadDirs.length, 2);
    assert.equal(c.linkedinMinIntervalMs, 20000);
  });
  it('refuses 0.0.0.0 unless BROWSER_ALLOW_REMOTE=true', () => {
    assert.throws(() => loadConfig({ BROWSER_WORKER_TOKEN: 'x', BROWSER_WORKER_HOST: '0.0.0.0' }), /BROWSER_ALLOW_REMOTE/);
    const c = loadConfig({ BROWSER_WORKER_TOKEN: 'x', BROWSER_WORKER_HOST: '0.0.0.0', BROWSER_ALLOW_REMOTE: 'true' });
    assert.equal(c.host, '0.0.0.0');
  });
  it('LinkedIn interval cannot go below 20 s', () => {
    const c = loadConfig({ BROWSER_WORKER_TOKEN: 'x', LINKEDIN_MIN_INTERVAL_MS: '10' });
    assert.equal(c.linkedinMinIntervalMs, 20000);
  });
  it('loopback detection', () => {
    for (const h of ['127.0.0.1', 'localhost', '::1', '[::1]', '127.1.2.3']) assert.ok(isLoopbackHost(h), h);
    for (const h of ['0.0.0.0', '::', '192.168.1.5', 'example.com']) assert.ok(!isLoopbackHost(h), h);
  });
});

describe('decideBlocker', () => {
  const probe = (o = {}) => ({ captcha: null, cloudflare: false, otp: null, mfa: null, passwordVisible: false, otherInputs: 0, accessDeniedText: false, tooManyText: false, ...o });
  it('maps statuses', () => {
    assert.equal(decideBlocker({ navError: 'net::ERR_CONNECTION_REFUSED' }).kind, 'network_error');
    assert.equal(decideBlocker({ status: 403, probe: probe() }).kind, 'access_denied');
    assert.equal(decideBlocker({ status: 401, probe: probe() }).kind, 'access_denied');
    assert.equal(decideBlocker({ status: 429, probe: probe() }).kind, 'rate_limited');
    assert.equal(decideBlocker({ status: 502, probe: probe() }).kind, 'site_error');
    assert.equal(decideBlocker({ status: 200, probe: probe() }), null);
  });
  it('captcha, otp, mfa, login', () => {
    assert.equal(decideBlocker({ status: 200, probe: probe({ captcha: 'x' }) }).kind, 'captcha');
    assert.equal(decideBlocker({ status: 403, probe: probe({ cloudflare: true }) }).kind, 'captcha');
    assert.equal(decideBlocker({ status: 200, probe: probe({ otp: 'x' }) }).kind, 'otp');
    assert.equal(decideBlocker({ status: 200, probe: probe({ mfa: 'x' }) }).kind, 'mfa');
    assert.equal(decideBlocker({ status: 200, url: 'https://www.linkedin.com/authwall?x=1', probe: probe() }).kind, 'login_required');
    assert.equal(decideBlocker({ status: 200, url: 'https://x.com/a', probe: probe({ passwordVisible: true }), fields: [] }).kind, 'login_required');
    // password plus a real application form is not a login wall
    const appFields = [{ type: 'text' }, { type: 'email' }, { type: 'file' }];
    assert.equal(decideBlocker({ status: 200, url: 'https://x.com/a', probe: probe({ passwordVisible: true }), fields: appFields }), null);
  });
});

describe('answer matching', () => {
  const fields = [
    { id: 'first_name', label: 'First Name', type: 'text' },
    { id: 'q_1', label: 'Why do you want to join Acme?', type: 'textarea' },
    { id: 'resume-upload-input', label: 'Resume/CV', type: 'file' },
    { id: 'cover', label: 'Cover Letter', type: 'file' },
  ];
  it('id, case-insensitive id, then label', () => {
    const { matches, unmatched } = matchKeys(fields, { FIRST_NAME: 'A', 'why do you want to join acme': 'B', nope: 1 });
    assert.equal(matches.get('first_name').value, 'A');
    assert.equal(matches.get('q_1').value, 'B');
    assert.deepEqual(unmatched, ['nope']);
  });
  it('fuzzy file keys', () => {
    const { matches } = matchKeys(fields, { resume: '/r.pdf', cover_letter: '/c.pdf' }, { fuzzy: true, types: ['file'] });
    assert.equal(matches.get('resume-upload-input').value, '/r.pdf');
    assert.equal(matches.get('cover').value, '/c.pdf');
  });
  it('chooseOption', () => {
    const opts = [
      { label: 'Yes', value: '1', index: 1 },
      { label: 'No', value: '0', index: 2 },
      { label: 'Decline To Self Identify', value: '3', index: 3 },
    ];
    assert.equal(chooseOption(opts, 'yes').index, 1);
    assert.equal(chooseOption(opts, true).index, 1);
    assert.equal(chooseOption(opts, false).index, 2);
    assert.equal(chooseOption(opts, '0').index, 2);
    assert.equal(chooseOption(opts, 'decline').index, 3);
    assert.equal(chooseOption(opts, 'maybe'), null);
  });
  it('helpers', () => {
    assert.ok(isTruthy('Yes') && isTruthy(true) && !isTruthy('no') && !isTruthy(false));
    assert.equal(screenshotName('app/1 2', 1729240000123), 'app_1_2-1729240000.png');
    assert.equal(cssString('a"b\\c'), '"a\\"b\\\\c"');
  });
});

describe('parseJsonLd', () => {
  it('spreads arrays, flattens @graph, skips invalid', () => {
    const out = parseJsonLd([
      '{"@type":"JobPosting","title":"X"}',
      '[{"@type":"Organization"},{"@type":"WebSite"}]',
      '{"@context":"https://schema.org","@graph":[{"@type":"BreadcrumbList"}]}',
      '{ nope',
    ]);
    assert.deepEqual(out.map((o) => o['@type']), ['JobPosting', 'Organization', 'WebSite', 'BreadcrumbList']);
    assert.equal(out[3]['@context'], 'https://schema.org');
  });
});

describe('uploads', () => {
  it('accepts files inside allowed dirs and refuses others', async () => {
    const ok = await resolveUpload(RESUME, [FIXTURES_DIR]);
    assert.equal(ok.ok, true);
    const tmp = await makeTmpDir();
    try {
      const outside = path.join(tmp, 'resume.pdf');
      await fs.copyFile(RESUME, outside);
      const r = await resolveUpload(outside, [FIXTURES_DIR]);
      assert.equal(r.ok, false);
      assert.match(r.reason, /outside/);
      const trav = await resolveUpload(path.join(FIXTURES_DIR, '..', 'server.mjs'), [FIXTURES_DIR]);
      assert.equal(trav.ok, false);
      assert.equal((await resolveUpload(path.join(FIXTURES_DIR, 'missing.pdf'), [FIXTURES_DIR])).ok, false);
      assert.equal((await resolveUpload(FIXTURES_DIR, [path.dirname(FIXTURES_DIR)])).ok, false); // directory
    } finally {
      await rmrf(tmp);
    }
  });
});

describe('sync primitives', () => {
  it('IntervalLimiter spaces calls', async () => {
    let t = 1000;
    const slept = [];
    const lim = new IntervalLimiter(20000, { now: () => t, sleepFn: async (ms) => { slept.push(ms); t += ms; } });
    await lim.wait();
    t += 5000;
    await lim.wait();
    await lim.wait();
    assert.deepEqual(slept, [15000, 20000]);
  });
  it('Semaphore limits concurrency', async () => {
    const sem = new Semaphore(3);
    let active = 0;
    let peak = 0;
    await Promise.all(
      Array.from({ length: 8 }, async () => {
        await sem.acquire();
        active++;
        peak = Math.max(peak, active);
        await new Promise((r) => setTimeout(r, 10));
        active--;
        sem.release();
      }),
    );
    assert.equal(peak, 3);
  });
  it('KeyedMutex serialises per key', async () => {
    const m = new KeyedMutex();
    const log = [];
    await Promise.all([
      m.run('a', async () => { log.push('a1'); await new Promise((r) => setTimeout(r, 20)); log.push('a1end'); }),
      m.run('a', async () => { log.push('a2'); }),
    ]);
    assert.deepEqual(log, ['a1', 'a1end', 'a2']);
  });
});

describe('misc', () => {
  it('detectAts', () => {
    assert.equal(detectAts('https://boards.greenhouse.io/acme/jobs/1'), 'greenhouse');
    assert.equal(detectAts('https://jobs.lever.co/acme/1/apply'), 'lever');
    assert.equal(detectAts('https://jobs.ashbyhq.com/acme'), 'ashby');
    assert.equal(detectAts('https://acme.wd1.myworkdayjobs.com/x'), 'workday');
    assert.equal(detectAts('https://www.linkedin.com/jobs/view/1'), 'linkedin');
    assert.equal(detectAts('http://127.0.0.1/x', { lever: true }), 'lever');
    assert.equal(detectAts('https://careers.example.com'), 'generic');
  });
  it('buildSearchUrl', () => {
    assert.equal(
      buildSearchUrl('https://www.linkedin.com', 'talent acquisition', 'Acme & Co'),
      'https://www.linkedin.com/search/results/people/?keywords=talent%20acquisition%20Acme%20%26%20Co',
    );
  });
  it('mock server multipart parser', async () => {
    const { parseMultipart } = await import('../../tests/mock-sites/server.mjs');
    const b = 'XyZ';
    const body = Buffer.from(
      `--${b}\r\nContent-Disposition: form-data; name="a"\r\n\r\n1\r\n` +
        `--${b}\r\nContent-Disposition: form-data; name="f"; filename="cv.pdf"\r\nContent-Type: application/pdf\r\n\r\n%PDF\r\n` +
        `--${b}--\r\n`,
      'latin1',
    );
    const r = parseMultipart(body, `multipart/form-data; boundary=${b}`);
    assert.deepEqual(r.fields, { a: '1' });
    assert.equal(r.files[0].filename, 'cv.pdf');
    assert.equal(r.files[0].size, 4);
    // keep the helper import used
    assert.equal(typeof startMock, 'function');
  });
});
