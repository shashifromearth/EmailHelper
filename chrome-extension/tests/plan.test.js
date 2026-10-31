import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  BREAKER_PAUSE_MS,
  EMPLOYER_PLATFORMS,
  GIVE_UP_MS,
  answerText,
  backoffDelay,
  bestFrame,
  blockerFrom,
  buildPlan,
  canAutoSubmit,
  chooseOption,
  claimPlatforms,
  eventFor,
  isJobBoard,
  jobFromLink,
  nextCheckAt,
  pausedPlatforms,
  publicField,
  recordOutcome,
  requiredLeft,
  shouldGiveUp,
  stuckReason,
  submitOutcome,
  toBase64,
} from '../src/plan.js';
import { boardForHost } from '../src/plan.js';

const opts = (...labels) => labels.map((label, index) => ({ label, value: label.toLowerCase(), index }));

test('options match by label, value, then shortest containing label', () => {
  const o = opts('Yes', 'No', 'Prefer not to say', 'United States', 'United States Minor Outlying Islands');
  assert.equal(chooseOption(o, true).label, 'Yes');
  assert.equal(chooseOption(o, 'no').label, 'No');
  assert.equal(chooseOption(o, 'United').label, 'United States');
  assert.equal(chooseOption(o, 'Mars'), null);
});

test('the plan turns answers into fill steps, resume uploads and questions for the person', () => {
  const fields = [
    { id: 'name', label: 'Full name', type: 'text' },
    { id: 'country', label: 'Country', type: 'select', _opts: opts('India', 'Germany') },
    { id: 'remote', label: 'Open to remote?', type: 'radio', _group: true, _buttons: true, _opts: opts('Yes', 'No') },
    { id: 'terms', label: 'I agree', type: 'checkbox', _group: false, _opts: [] },
    { id: 'stack', label: 'Languages', type: 'checkbox', _group: true, _opts: opts('Rust', 'Go', 'Java') },
    { id: 'cv', label: 'Resume', type: 'file' },
    { id: 'salary', label: 'Salary', type: 'text' },
    { id: 'city', label: 'City', type: 'select', _opts: opts('Pune') },
  ];
  const plan = buildPlan(fields, [
    { kind: 'value', id: 'name', value: 'Asha Rao' },
    { kind: 'value', id: 'country', value: 'india' },
    { kind: 'value', id: 'remote', value: true },
    { kind: 'value', id: 'terms', value: 'yes' },
    { kind: 'value', id: 'stack', value: ['Rust', 'Go'] },
    { kind: 'resume', id: 'cv' },
    { kind: 'ask', id: 'salary', reason: 'sensitive question' },
    { kind: 'value', id: 'city', value: 'Delhi' },
    { kind: 'skip', id: 'missing' },
  ]);
  assert.deepEqual(plan.steps, [
    { id: 'name', type: 'text', text: 'Asha Rao' },
    { id: 'country', type: 'select', indexes: [0], buttons: false },
    { id: 'remote', type: 'radio', indexes: [0], buttons: true },
    { id: 'terms', type: 'checkbox', checked: true },
    { id: 'stack', type: 'checkbox', indexes: [0, 1] },
  ]);
  assert.deepEqual(plan.resume, ['cv']);
  assert.deepEqual(plan.asks, [{ id: 'salary', label: 'Salary', reason: 'sensitive question' }]);
  assert.deepEqual(plan.problems, [{ id: 'city', reason: 'no option matching "Delhi"' }]);
});

test('the agent sees only public field data', () => {
  const f = { id: 'cv', label: 'Resume', type: 'file', required: true, options: [], accept: '.pdf', _opts: [], _group: false };
  assert.deepEqual(publicField(f), { id: 'cv', label: 'Resume', type: 'file', required: true, options: [], accept: '.pdf' });
});

test('a queued link becomes job details', () => {
  const job = jobFromLink({ url: 'https://x/1', title: 'Rust Engineer', company: 'Acme', remote: 'unknown', skills: ['rust'] });
  assert.equal(job.title, 'Rust Engineer');
  assert.equal(job.remote, null);
  assert.deepEqual(job.skills, ['rust']);
  assert.deepEqual(jobFromLink(null), {});
});

test('blockers: checks and sign-in stay with the person', () => {
  assert.equal(blockerFrom({ captcha: 'captcha iframe' }, 'https://x/job', []).kind, 'captcha');
  assert.equal(blockerFrom({}, 'https://www.linkedin.com/authwall?x', []).kind, 'login_required');
  assert.equal(blockerFrom({ passwordVisible: true }, 'https://x/register', [{ type: 'email' }]).kind, 'login_required');
  assert.equal(blockerFrom({ passwordVisible: true }, 'https://x/apply', [{ type: 'file' }]), null);
  assert.equal(blockerFrom({}, 'https://boards.greenhouse.io/acme/jobs/1', []), null);
});

test('the frame with an application form wins', () => {
  const r = (frameId, appLike, n) => ({ frameId, result: { appLike, fields: Array(n).fill({}) } });
  assert.equal(bestFrame([r(0, false, 9), r(3, true, 4), r(5, true, 2), { frameId: 7, result: null }]).frameId, 3);
  assert.equal(bestFrame([]), null);
});

test('base64 of large files', () => {
  const bytes = new Uint8Array(100_000).map((_, i) => i % 256);
  assert.equal(toBase64(bytes), Buffer.from(bytes).toString('base64'));
});

test('what the person answered becomes text for the agent to remember', () => {
  assert.equal(answerText(' 30 days '), '30 days');
  assert.equal(answerText({ picked: [1] }, ['Yes', 'No']), 'No');
  assert.equal(answerText({ picked: [0, 2] }, ['Rust', 'Go', 'Java']), 'Rust, Java');
  assert.equal(answerText({ picked: [NaN] }, ['Yes']), '');
  assert.equal(answerText(undefined), '');
});

test('job boards are never auto-submitted', () => {
  for (const p of ['linkedin', 'naukri', 'glassdoor', 'wellfound', 'indeed', 'LinkedIn']) assert.equal(isJobBoard(p), true);
  for (const p of EMPLOYER_PLATFORMS) assert.equal(isJobBoard(p), false);
});

test('claims: auto-submit keeps to employer sites, paused platforms are left out', () => {
  assert.equal(claimPlatforms({ platforms: [], autoSubmit: false }), null);
  assert.deepEqual(claimPlatforms({ platforms: [], autoSubmit: true }), EMPLOYER_PLATFORMS);
  assert.deepEqual(claimPlatforms({ platforms: ['lever', 'linkedin'], autoSubmit: true, paused: ['lever'] }), ['linkedin']);
  const any = claimPlatforms({ platforms: [], autoSubmit: false, paused: ['lever'] });
  assert.ok(any.includes('linkedin') && any.includes('greenhouse') && !any.includes('lever'));
  assert.deepEqual(claimPlatforms({ platforms: ['lever'], autoSubmit: true, paused: ['lever'] }), []);
});

test('auto-submit only when nothing needs the person', () => {
  const ok = { platform: 'greenhouse', asks: [], problems: [], blocker: null, submitFound: true, autoSubmit: true };
  assert.equal(canAutoSubmit(ok), true);
  assert.equal(canAutoSubmit({ ...ok, autoSubmit: false }), false);
  assert.equal(canAutoSubmit({ ...ok, platform: 'linkedin' }), false);
  assert.equal(canAutoSubmit({ ...ok, platform: null }), false);
  assert.equal(canAutoSubmit({ ...ok, blocker: { kind: 'captcha' } }), false);
  assert.equal(canAutoSubmit({ ...ok, submitFound: false }), false);
  assert.equal(canAutoSubmit({ ...ok, asks: [{ id: 'salary', required: true }] }), false);
  assert.equal(canAutoSubmit({ ...ok, asks: [{ id: 'pronouns', required: false }] }), true);
  assert.equal(canAutoSubmit({ ...ok, problems: [{ id: 'city', reason: 'no option' }] }), false);
});

test('stuck: blockers, errors, missing forms, required questions, no submit button', () => {
  const base = { phase: 'review', platform: 'lever', asks: [], problems: [], submitFound: true };
  assert.equal(stuckReason(base), null);
  assert.equal(stuckReason({ ...base, blocker: { kind: 'login_required', detail: 'sign-in page' } }).kind, 'login_required');
  assert.deepEqual(stuckReason({ ...base, phase: 'error', status: 'agent: timed out' }), { kind: 'error', reason: 'agent: timed out' });
  assert.equal(stuckReason({ ...base, phase: 'waiting' }).kind, 'no_form');
  assert.equal(stuckReason({ ...base, phase: 'opening' }), null);
  assert.deepEqual(stuckReason({ ...base, asks: [{ label: 'Salary', required: true }, { label: 'Pronouns', required: false }] }), {
    kind: 'questions',
    reason: 'Needs answers: Salary',
  });
  assert.equal(stuckReason({ ...base, submitFound: false }).kind, 'no_submit');
  assert.equal(stuckReason({ ...base, platform: 'indeed', submitFound: false }), null);
});

test('required fields still empty become questions', () => {
  const fields = [
    { id: 'a', label: 'Name', required: true },
    { id: 'b', label: 'Phone', required: true },
    { id: 'c', label: 'Blog', required: false },
    { id: 'd', label: 'Salary', required: true },
  ];
  const states = { a: { empty: false }, b: { empty: true }, c: { empty: true }, d: { empty: true } };
  assert.deepEqual(requiredLeft(fields, states, new Set(['d'])), [{ id: 'b', label: 'Phone', reason: 'still empty', problem: false }]);
});

test('submit outcome: confirmation text, form gone, next step, validation errors', () => {
  const page = (o) => ({ text: '', formGone: false, errors: [], inputs: 0, ...o });
  const form = [page({ text: 'Apply for Rust Engineer', inputs: 9 })];
  assert.equal(submitOutcome(form, [page({ text: 'Thank you for applying!' })]).result, 'confirmed');
  assert.equal(submitOutcome(form, [page({ text: 'Your application has been received' })]).result, 'confirmed');
  assert.equal(submitOutcome(form, [page({ text: 'We have received your application' })]).result, 'confirmed');
  assert.equal(submitOutcome(form, [page({ formGone: true }), page({ formGone: true })]).result, 'confirmed');
  assert.equal(submitOutcome(form, [page({ formGone: true, inputs: 5 })]).result, 'next_step');
  assert.deepEqual(submitOutcome(form, [page({ errors: ['Phone is required', 'Phone is required'] })]), {
    result: 'failed',
    errors: ['Phone is required'],
  });
  assert.equal(submitOutcome(form, [page({ formGone: true, errors: ['Something went wrong'] })]).result, 'failed');
  // "thank you" already on the form page is not a confirmation
  const thanks = [page({ text: 'Thank you for your interest in Acme', inputs: 9 })];
  assert.equal(submitOutcome(thanks, thanks).result, 'failed');
});

test('backoff: 30 s doubling to 480 s, then 15 minutes; give up after 2 hours', () => {
  assert.deepEqual([0, 1, 2, 3, 4, 5, 6, 20].map(backoffDelay), [30, 60, 120, 240, 480, 900, 900, 900]);
  const startedAt = 1_000_000;
  assert.equal(nextCheckAt({ attempt: 0, startedAt }, startedAt), startedAt + 30_000);
  assert.equal(nextCheckAt({ attempt: 9, startedAt }, startedAt + GIVE_UP_MS - 60_000), startedAt + GIVE_UP_MS);
  assert.equal(shouldGiveUp({ startedAt }, startedAt + GIVE_UP_MS - 1), false);
  assert.equal(shouldGiveUp({ startedAt }, startedAt + GIVE_UP_MS), true);
});

test('circuit breaker: three failures in a row pause a platform; a success resets', () => {
  const now = 5_000;
  let b = {};
  b = recordOutcome(b, 'lever', false, now);
  b = recordOutcome(b, 'lever', false, now);
  assert.deepEqual(pausedPlatforms(b, now), {});
  b = recordOutcome(b, 'lever', true, now);
  b = recordOutcome(b, 'lever', false, now);
  b = recordOutcome(b, 'lever', false, now);
  assert.deepEqual(pausedPlatforms(b, now), {});
  b = recordOutcome(b, 'lever', false, now);
  assert.deepEqual(pausedPlatforms(b, now), { lever: now + BREAKER_PAUSE_MS });
  assert.deepEqual(pausedPlatforms(b, now + BREAKER_PAUSE_MS), {});
  assert.equal(recordOutcome(b, null, false, now), b);
});

test('events carry the job, platform and person', () => {
  const cur = { link: { id: 'l1', url: 'https://x/1', platform: 'lever' }, person: 'asha' };
  assert.deepEqual(eventFor(cur, 'fill_failed', { field: 'City', reason: 'no option' }), {
    kind: 'fill_failed',
    link_id: 'l1',
    url: 'https://x/1',
    platform: 'lever',
    person: 'asha',
    field: 'City',
    reason: 'no option',
    detail: null,
  });
  assert.equal(eventFor({ link: null, url: 'https://y', person: null }, 'error').url, 'https://y');
});

test('a page on a job board is never auto-submitted, even behind an employer link', () => {
  assert.equal(boardForHost('www.linkedin.com'), 'linkedin');
  assert.equal(boardForHost('in.indeed.com'), 'indeed');
  assert.equal(boardForHost('www.glassdoor.co.in'), 'glassdoor');
  assert.equal(boardForHost('www.naukri.com'), 'naukri');
  assert.equal(boardForHost('boards.greenhouse.io'), null);
  assert.equal(boardForHost('notlinkedin.com'), null);
});
