// End-to-end API tests: real headless Chromium against the mock sites.
import { describe, it, before, after, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { startWorker, startMock, RESUME, makeTmpDir, rmrf } from './helpers.js';

let w;
let mock;
const byId = (fields) => Object.fromEntries(fields.map((f) => [f.id, f]));
const subs = async () => (await (await fetch(`${mock.url}/__submissions`)).json()).submissions;

before(async () => {
  mock = await startMock();
  w = await startWorker();
});
after(async () => {
  await w?.close();
  await mock?.close();
});

describe('health and auth', () => {
  it('GET /health needs no token', async () => {
    const r = await w.call('GET', '/health', undefined, { token: null });
    assert.equal(r.status, 200);
    assert.equal(r.body.status, 'ok');
    assert.equal(r.body.browser, 'chromium');
    assert.equal(r.body.headless, true);
    assert.equal(r.body.version, '1.0.0');
  });
  it('401 without or with a wrong token', async () => {
    const a = await w.call('POST', '/v1/extract', { url: `${mock.url}/greenhouse/job.html` }, { token: null });
    assert.equal(a.status, 401);
    assert.equal(a.body.error, 'unauthorized');
    const b = await w.call('POST', '/v1/extract', { url: `${mock.url}/greenhouse/job.html` }, { token: 'wrong' });
    assert.equal(b.status, 401);
  });
  it('404 / 405 / 400 / 413', async () => {
    assert.equal((await w.call('GET', '/nope')).status, 404);
    assert.equal((await w.call('GET', '/v1/extract')).status, 405);
    const bad = await w.call('POST', '/v1/extract', { url: 'file:///etc/passwd' });
    assert.equal(bad.status, 400);
    assert.equal(bad.body.error, 'validation_failed');
    assert.equal((await w.call('POST', '/v1/extract', '{not json', { raw: true })).status, 400);
    assert.equal((await w.call('POST', '/v1/apply/inspect', { url: `${mock.url}/x` })).status, 400); // no session_key
    const big = JSON.stringify({ url: `${mock.url}/greenhouse/job.html`, pad: 'x'.repeat(1024 * 1024 + 10) });
    assert.equal((await w.call('POST', '/v1/extract', big, { raw: true })).status, 413);
  });
  it('/v1/session/login returns 409 in headless mode', async () => {
    const r = await w.call('POST', '/v1/session/login', { url: 'https://www.linkedin.com/login' });
    assert.equal(r.status, 409);
    assert.equal(r.body.error, 'headless_mode');
    assert.match(r.body.message, /BROWSER_HEADLESS=false/);
  });
});

describe('extract', () => {
  it('returns text, title, JSON-LD and absolute links', async () => {
    const r = await w.call('POST', '/v1/extract', { url: `${mock.url}/greenhouse/job.html` });
    assert.equal(r.status, 200, r.text);
    const b = r.body;
    assert.equal(b.blocker, null);
    assert.equal(b.final_url, `${mock.url}/greenhouse/job.html`);
    assert.match(b.title, /Staff AI Engineer/);
    assert.match(b.text, /retrieval and agent systems/);
    assert.ok(!/\s{2,}/.test(b.text), 'whitespace collapsed');
    assert.deepEqual(b.json_ld.map((x) => x['@type']), ['JobPosting', 'Organization', 'BreadcrumbList']);
    assert.equal(b.json_ld[0].hiringOrganization.name, 'Acme');
    const apply = b.links.find((l) => l.text === 'Apply for this job');
    assert.equal(apply.href, `${mock.url}/greenhouse/apply.html`);
    assert.ok(b.links.every((l) => /^https?:\/\//.test(l.href)));
  });
  it('maps HTTP errors and network failures to blockers', async () => {
    const k = async (p) => (await w.call('POST', '/v1/extract', { url: p })).body.blocker?.kind;
    assert.equal(await k(`${mock.url}/status/403`), 'access_denied');
    assert.equal(await k(`${mock.url}/status/429`), 'rate_limited');
    assert.equal(await k(`${mock.url}/status/503`), 'site_error');
    assert.equal(await k('http://127.0.0.1:1/'), 'network_error');
  });
  it('handles concurrent requests (queue beyond 3 pages)', async () => {
    const rs = await Promise.all(
      Array.from({ length: 5 }, () => w.call('POST', '/v1/extract', { url: `${mock.url}/lever/apply.html` })),
    );
    assert.ok(rs.every((r) => r.status === 200 && r.body.blocker === null));
  });
});

describe('inspect', () => {
  it('greenhouse: follows the Apply link and lists fields', async () => {
    const r = await w.call('POST', '/v1/apply/inspect', { session_key: 'gh_inspect', url: `${mock.url}/greenhouse/job.html` });
    assert.equal(r.status, 200, r.text);
    const b = r.body;
    assert.equal(b.blocker, null);
    assert.equal(b.ats, 'greenhouse');
    assert.equal(b.final_url, `${mock.url}/greenhouse/apply.html`);
    assert.deepEqual(b.fields.map((f) => f.id), [
      'first_name',
      'last_name',
      'email',
      'phone',
      'resume',
      'job_application_answers_attributes_0_text_value',
      'job_application_answers_attributes_1_boolean_value',
      'job_application_answers_attributes_2_text_value',
      'job_application_gender',
      'job_application_veteran_status',
    ]);
    const f = byId(b.fields);
    assert.deepEqual(f.first_name, { id: 'first_name', label: 'First Name', type: 'text', required: true, options: [] });
    assert.equal(f.email.type, 'email');
    assert.equal(f.phone.type, 'tel');
    assert.equal(f.phone.required, false);
    assert.deepEqual(f.resume, { id: 'resume', label: 'Resume/CV', type: 'file', required: true, options: [], accept: '.pdf,.doc,.docx,.txt,.rtf' });
    assert.equal(f.job_application_answers_attributes_0_text_value.label, 'LinkedIn Profile');
    const auth = f.job_application_answers_attributes_1_boolean_value;
    assert.equal(auth.label, 'Are you legally authorized to work in India?');
    assert.equal(auth.type, 'select');
    assert.equal(auth.required, true);
    assert.deepEqual(auth.options, ['Yes', 'No']);
    const why = f.job_application_answers_attributes_2_text_value;
    assert.deepEqual([why.label, why.type, why.required], ['Why do you want to join Acme?', 'textarea', true]);
    assert.equal(f.job_application_gender.required, false);
    assert.deepEqual(f.job_application_gender.options, ['Male', 'Female', 'Decline To Self Identify']);
  });

  it('lever: radio and checkbox groups, wrapping labels', async () => {
    const r = await w.call('POST', '/v1/apply/inspect', { session_key: 'lever_inspect', url: `${mock.url}/lever/apply.html` });
    const b = r.body;
    assert.equal(b.blocker, null, r.text);
    assert.equal(b.ats, 'lever');
    assert.deepEqual(b.fields.map((f) => f.id), [
      'resume-upload-input',
      'name',
      'email',
      'phone',
      'org',
      'urls[LinkedIn]',
      'urls[GitHub]',
      'cards[c1a1][field0]',
      'cards[c1a1][field1]',
      'comments',
      'consent[store]',
    ]);
    const f = byId(b.fields);
    assert.deepEqual([f['resume-upload-input'].label, f['resume-upload-input'].type, f['resume-upload-input'].required], ['Resume/CV', 'file', true]);
    assert.deepEqual([f.name.label, f.name.required], ['Full name', true]);
    assert.deepEqual([f.phone.label, f.phone.type, f.phone.required], ['Phone', 'text', false]);
    assert.equal(f['urls[LinkedIn]'].label, 'LinkedIn URL');
    assert.deepEqual(f['cards[c1a1][field0]'], {
      id: 'cards[c1a1][field0]',
      label: 'Are you willing to relocate to Bengaluru?',
      type: 'radio',
      required: true,
      options: ['Yes', 'No', 'Open to discussion'],
    });
    assert.deepEqual(f['cards[c1a1][field1]'], {
      id: 'cards[c1a1][field1]',
      label: 'Which languages do you use professionally?',
      type: 'checkbox',
      required: false,
      options: ['Rust', 'Python', 'Go'],
    });
    assert.equal(f.comments.type, 'textarea');
    assert.equal(f.comments.label, 'Additional information');
    assert.deepEqual([f['consent[store]'].type, f['consent[store]'].label], ['checkbox', 'I agree to Acme storing my data for future opportunities']);
  });

  it('generic career portal: label fallbacks and field_<n>', async () => {
    const r = await w.call('POST', '/v1/apply/inspect', { session_key: 'career_inspect', url: `${mock.url}/career/apply.html` });
    const b = r.body;
    assert.equal(b.blocker, null, r.text);
    assert.equal(b.ats, 'generic');
    const ids = b.fields.map((f) => f.id);
    assert.deepEqual(ids, ['fullName', 'email', 'mobile', 'expected_ctc', 'notice_period', 'current_location', 'cv', 'cover_letter', 'field_8']);
    const f = byId(b.fields);
    assert.deepEqual([f.fullName.label, f.fullName.required], ['Full Name', true]); // label[for] + "*"
    assert.deepEqual([f.email.label, f.email.required], ['Email address', true]); // wrapping label + required attr
    assert.equal(f.mobile.label, 'Mobile number'); // placeholder
    assert.deepEqual([f.expected_ctc.label, f.expected_ctc.required], ['What is your expected CTC?', true]); // preceding question text
    assert.deepEqual([f.notice_period.label, f.notice_period.required], ['Notice period', true]); // aria-labelledby + aria-required
    assert.deepEqual(f.notice_period.options, ['Immediate', '15 days', '30 days', '60 days', '90 days']);
    assert.equal(f.current_location.label, 'Current location'); // aria-label
    assert.equal(f.cv.accept, 'application/pdf');
    assert.equal(f.cover_letter.label, 'cover_letter'); // name fallback
    assert.equal(f.field_8.type, 'number');
    assert.ok(!ids.includes('website_url'), 'honeypot skipped');
  });

  it('detects captcha, otp and login walls', async () => {
    const i = async (p, key) => (await w.call('POST', '/v1/apply/inspect', { session_key: key, url: `${mock.url}${p}` })).body;
    const cap = await i('/blocked/captcha.html', 'b1');
    assert.equal(cap.blocker.kind, 'captcha');
    assert.equal(cap.fields.length, 3);
    assert.equal((await i('/blocked/otp.html', 'b2')).blocker.kind, 'otp');
    const login = await i('/blocked/login.html', 'b3');
    assert.equal(login.blocker.kind, 'login_required');
    assert.deepEqual(login.fields, []);
    assert.equal((await i('/greenhouse/about-missing.html', 'b4')).blocker.kind, 'unexpected_form');
  });
});

describe('fill', () => {
  const ghAnswers = {
    first_name: 'Asha',
    last_name: 'Example',
    email: 'asha@example.com',
    phone: '+91 98765 43210',
    'LinkedIn Profile': 'https://www.linkedin.com/in/asha-example',
    job_application_answers_attributes_1_boolean_value: 'Yes',
    'Why do you want to join Acme?': 'I enjoy building reliable LLM systems.',
    job_application_gender: 'decline',
  };
  beforeEach(async () => {
    await fetch(`${mock.url}/__reset`, { method: 'POST' });
  });

  it('without submit: reports missing_required and takes a screenshot', async () => {
    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'gh_partial',
      url: `${mock.url}/greenhouse/apply.html`,
      answers: { first_name: 'Asha', last_name: 'Example', email: 'asha@example.com', unknown_question: 'x' },
      files: {},
      submit: false,
    });
    assert.equal(r.status, 200, r.text);
    const b = r.body;
    assert.equal(b.status, 'filled');
    assert.equal(b.blocker, null);
    assert.deepEqual(b.missing_required.sort(), [
      'job_application_answers_attributes_1_boolean_value',
      'job_application_answers_attributes_2_text_value',
      'resume',
    ]);
    assert.equal(b.confirmation_text, null);
    assert.match(b.screenshot_path, /gh_partial-\d+\.png$/);
    await fs.access(path.join(w.cfg.screenshotDir, path.basename(b.screenshot_path)));
    assert.deepEqual(b.filled.sort(), ['email', 'first_name', 'last_name']);
    assert.ok(b.skipped.some((s) => s.key === 'unknown_question'));
  });

  it('refuses to submit while required fields are missing', async () => {
    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'gh_refuse',
      url: `${mock.url}/greenhouse/apply.html`,
      answers: { first_name: 'Asha' },
      submit: true,
    });
    assert.equal(r.body.status, 'filled');
    assert.ok(r.body.missing_required.length > 0);
    assert.equal((await subs()).length, 0);
  });

  it('greenhouse: inspect, then fill + upload + submit', async () => {
    const ins = await w.call('POST', '/v1/apply/inspect', { session_key: 'gh_full', url: `${mock.url}/greenhouse/apply.html` });
    assert.equal(ins.body.blocker, null);
    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'gh_full',
      url: `${mock.url}/greenhouse/apply.html`,
      answers: ghAnswers,
      files: { resume: RESUME },
      submit: true,
    });
    const b = r.body;
    assert.equal(b.status, 'submitted', r.text);
    assert.deepEqual(b.missing_required, []);
    assert.equal(b.blocker, null);
    assert.match(b.confirmation_text, /thank you for applying/i);
    assert.equal(b.final_url, `${mock.url}/submit?ats=greenhouse`);
    assert.ok(b.screenshot_path);
    const s = await subs();
    assert.equal(s.length, 1);
    const sub = s[0];
    assert.equal(sub.contentType, 'multipart/form-data');
    assert.equal(sub.fields['job_application[first_name]'], 'Asha');
    assert.equal(sub.fields['job_application[email]'], 'asha@example.com');
    assert.equal(sub.fields['job_application[answers_attributes][0][text_value]'], 'https://www.linkedin.com/in/asha-example');
    assert.equal(sub.fields['job_application[answers_attributes][1][boolean_value]'], '1');
    assert.equal(sub.fields['job_application[gender]'], '3');
    assert.deepEqual(sub.files.map((f) => [f.field, f.filename]), [['job_application[resume]', 'resume.pdf']]);
    assert.ok(sub.files[0].size > 100);
  });

  it('lever: radio, checkbox group, single checkbox, upload, submit', async () => {
    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'lever_full',
      url: `${mock.url}/lever/apply.html`,
      answers: {
        name: 'Asha Example',
        email: 'asha@example.com',
        phone: '+91 98765 43210',
        org: 'Initech',
        'urls[LinkedIn]': 'https://www.linkedin.com/in/asha-example',
        'cards[c1a1][field0]': 'yes',
        'cards[c1a1][field1]': ['Rust', 'Python'],
        'consent[store]': true,
        comments: 'Happy to share more.',
      },
      files: { resume: RESUME },
      submit: true,
    });
    const b = r.body;
    assert.equal(b.status, 'submitted', r.text);
    assert.match(b.confirmation_text, /application submitted|received/i);
    const s = await subs();
    assert.equal(s.length, 1);
    const sub = s[0];
    assert.equal(sub.ats, 'lever');
    assert.equal(sub.fields.name, 'Asha Example');
    assert.equal(sub.fields['cards[c1a1][field0]'], 'Yes');
    assert.deepEqual(sub.fields['cards[c1a1][field1]'], ['Rust', 'Python']);
    assert.equal(sub.fields['consent[store]'], 'true');
    assert.deepEqual(sub.files.map((f) => [f.field, f.filename]), [['resume', 'resume.pdf']]);
  });

  it('refuses uploads outside BROWSER_ALLOWED_UPLOAD_DIRS (upload_failure)', async () => {
    const tmp = await makeTmpDir();
    try {
      const outside = path.join(tmp, 'resume.pdf');
      await fs.copyFile(RESUME, outside);
      const r = await w.call('POST', '/v1/apply/fill', {
        session_key: 'gh_outside',
        url: `${mock.url}/greenhouse/apply.html`,
        answers: ghAnswers,
        files: { resume: outside },
        submit: true,
      });
      assert.equal(r.body.status, 'blocked', r.text);
      assert.equal(r.body.blocker.kind, 'upload_failure');
      assert.match(r.body.blocker.detail, /outside BROWSER_ALLOWED_UPLOAD_DIRS/);
      assert.ok(r.body.missing_required.includes('resume'));
      assert.equal((await subs()).length, 0);
    } finally {
      await rmrf(tmp);
    }
  });

  it('never submits a page with a captcha', async () => {
    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'cap_fill',
      url: `${mock.url}/blocked/captcha.html`,
      answers: { name: 'Asha Example', email: 'asha@example.com' },
      submit: true,
    });
    assert.equal(r.body.status, 'blocked');
    assert.equal(r.body.blocker.kind, 'captcha');
    assert.deepEqual(r.body.missing_required, []);
    assert.ok(r.body.screenshot_path);
    assert.equal((await subs()).length, 0);
  });

  it('otp and login pages are blocked before filling', async () => {
    const otp = await w.call('POST', '/v1/apply/fill', { session_key: 'otp_fill', url: `${mock.url}/blocked/otp.html`, answers: { code: '123456' }, submit: true });
    assert.equal(otp.body.status, 'blocked');
    assert.equal(otp.body.blocker.kind, 'otp');
    const login = await w.call('POST', '/v1/apply/fill', { session_key: 'login_fill', url: `${mock.url}/blocked/login.html`, answers: {}, submit: true });
    assert.equal(login.body.status, 'blocked');
    assert.equal(login.body.blocker.kind, 'login_required');
  });
});

describe('modern ATS widgets', () => {
  it('ashby: finds the real form behind the autofill box, incl. yes/no buttons and a search combobox', async () => {
    const url = `${mock.url}/ashby/application.html`;
    const ins = await w.call('POST', '/v1/apply/inspect', { session_key: 'ashby_widgets', url });
    assert.equal(ins.status, 200, ins.text);
    assert.equal(ins.body.blocker, null);
    const f = byId(ins.body.fields);
    assert.ok(f._systemfield_name && f._systemfield_email && f._systemfield_resume, JSON.stringify(ins.body.fields));
    const loc = ins.body.fields.find((x) => x.type === 'combobox');
    assert.equal(loc.label, 'Where are you currently located?');
    assert.deepEqual(loc.options, [], 'search box shows nothing until typed');
    assert.equal(f.relocate.type, 'radio');
    assert.deepEqual(f.relocate.options, ['Yes', 'No']);
    assert.ok(!ins.body.fields.some((x) => x.type === 'checkbox'), 'hidden checkbox behind the buttons is not a field');

    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'ashby_widgets',
      url,
      answers: {
        _systemfield_name: 'Asha Example',
        _systemfield_email: 'asha@example.com',
        [loc.id]: 'Bengaluru, India',
        relocate: 'Yes',
      },
      submit: false,
    });
    assert.equal(r.body.status, 'filled', r.text);
    assert.deepEqual(r.body.missing_required, ['_systemfield_resume'], JSON.stringify(r.body));
    assert.deepEqual(r.body.skipped, [], JSON.stringify(r.body.skipped));
  });

  it('greenhouse new board: reads combobox choices on inspect and picks them on fill', async () => {
    const url = `${mock.url}/greenhouse/new-board.html`;
    const ins = await w.call('POST', '/v1/apply/inspect', { session_key: 'gh_new', url });
    const f = byId(ins.body.fields);
    assert.equal(f.country.type, 'combobox');
    assert.deepEqual(f.country.options, ['India', 'Indonesia', 'Ireland', 'United States']);
    assert.deepEqual(f.question_1.options, ['Yes', 'No']);
    assert.equal(f.question_1.required, true);

    const r = await w.call('POST', '/v1/apply/fill', {
      session_key: 'gh_new',
      url,
      answers: { first_name: 'Asha', email: 'asha@example.com', country: 'India', question_1: 'Yes' },
      submit: false,
    });
    assert.equal(r.body.status, 'filled', r.text);
    assert.deepEqual(r.body.missing_required.sort(), ['question_2', 'resume'], JSON.stringify(r.body));
  });
});
