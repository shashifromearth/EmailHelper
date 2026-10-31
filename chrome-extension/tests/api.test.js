import { test } from 'node:test';
import assert from 'node:assert/strict';

import { ApiError, createApi, DEFAULTS } from '../src/api.js';

function fakeFetch(routes) {
  const calls = [];
  const impl = async (url, init) => {
    calls.push({ url, init, body: init.body ? JSON.parse(init.body) : undefined });
    const r = routes[`${init.method} ${new URL(url).pathname}`];
    if (!r) return new Response('{"error":"not_found","message":"no route"}', { status: 404 });
    return typeof r === 'function' ? r() : new Response(JSON.stringify(r), { status: 200 });
  };
  return { impl, calls };
}

const settings = { ...DEFAULTS, apiToken: 'agent-token', linksToken: 'links-token', person: 'asha', platforms: ['lever'] };

test('claim asks for one job for this person and platforms, with the job-links token', async () => {
  const { impl, calls } = fakeFetch({ 'POST /v1/links/claim': { links: [{ id: 'l1', url: 'https://x' }] } });
  const link = await createApi(settings, impl).claim('chrome-1');
  assert.equal(link.id, 'l1');
  assert.equal(calls[0].url, 'http://127.0.0.1:7810/v1/links/claim');
  assert.equal(calls[0].init.headers['x-joblinks-token'], 'links-token');
  assert.deepEqual(calls[0].body, { client: 'chrome-1', limit: 1, lease_secs: 3600, person: 'asha', platforms: ['lever'] });
});

test('an empty queue is null', async () => {
  const { impl } = fakeFetch({ 'POST /v1/links/claim': { links: [] } });
  assert.equal(await createApi(settings, impl).claim('c'), null);
});

test('answers go to the agent with the API token', async () => {
  const { impl, calls } = fakeFetch({ 'POST /api/v1/apply/answers': { person: 'asha', answers: [] } });
  await createApi(settings, impl).answers('asha', { title: 'Dev' }, [{ id: 'a' }]);
  assert.equal(calls[0].init.headers['x-emailhelper-token'], 'agent-token');
  assert.deepEqual(calls[0].body, { person: 'asha', job: { title: 'Dev' }, fields: [{ id: 'a' }] });
});

test('the resume is picked by skills, then downloaded', async () => {
  const { impl, calls } = fakeFetch({
    'POST /v1/people/asha/resume': { file: 'resume.pdf', stack: 'golang', download: '/v1/people/asha/resumes/resume.pdf' },
    'GET /v1/people/asha/resumes/resume.pdf': () =>
      new Response(new Uint8Array([37, 80, 68, 70]), { status: 200, headers: { 'content-type': 'application/pdf' } }),
  });
  const r = await createApi(settings, impl).resume('asha', ['go']);
  assert.equal(r.file, 'resume.pdf');
  assert.equal(r.type, 'application/pdf');
  assert.deepEqual([...r.bytes], [37, 80, 68, 70]);
  assert.deepEqual(calls[0].body, { skills: ['go'], purpose: 'job_seeking' });
  assert.equal(calls[1].init.headers['x-knowledge-token'], 'agent-token');
});

test('service errors carry the service name and message', async () => {
  const { impl } = fakeFetch({});
  await assert.rejects(createApi(settings, impl).stats(), (e) => e instanceof ApiError && e.status === 404 && e.message === 'job-links: no route');
  const down = async () => {
    throw new TypeError('fetch failed');
  };
  await assert.rejects(createApi(settings, down).stats(), /job-links: not reachable/);
});

test('answers the person gave go to the agent', async () => {
  const { impl, calls } = fakeFetch({ 'POST /api/v1/apply/remember': { remembered: 1 } });
  await createApi(settings, impl).remember('asha', [{ label: 'Notice period', value: '30 days' }]);
  assert.deepEqual(calls[0].body, { person: 'asha', answers: [{ label: 'Notice period', value: '30 days' }] });
});

test('claim takes an allow-list of platforms; null claims any', async () => {
  const { impl, calls } = fakeFetch({ 'POST /v1/links/claim': { links: [] } });
  const api = createApi(settings, impl);
  await api.claim('c', ['greenhouse', 'lever']);
  await api.claim('c', null);
  assert.deepEqual(calls[0].body.platforms, ['greenhouse', 'lever']);
  assert.equal(calls[1].body.platforms, undefined);
});

test('answers are asked a few fields at a time and merged', async () => {
  let n = 0;
  const { impl, calls } = fakeFetch({
    'POST /api/v1/apply/answers': () => {
      n += 1;
      return new Response(JSON.stringify({ person: 'asha', answers: [{ id: `a${n}`, kind: 'skip' }] }), { status: 200 });
    },
  });
  const fields = Array.from({ length: 13 }, (_, i) => ({ id: `f${i}` }));
  const r = await createApi({ ...settings, person: '' }, impl).answers(null, { title: 'Dev' }, fields);
  assert.equal(calls.length, 3);
  assert.deepEqual(calls.map((c) => c.body.fields.length), [6, 6, 1]);
  assert.deepEqual(calls.map((c) => c.body.person), [null, 'asha', 'asha']);
  assert.deepEqual(r, { person: 'asha', answers: [1, 2, 3].map((i) => ({ id: `a${i}`, kind: 'skip' })) });
});

test('escalations: create, check and resolve on the agent', async () => {
  const { impl, calls } = fakeFetch({
    'POST /api/v1/apply/escalations': { id: 'e 1', telegram: true },
    'GET /api/v1/apply/escalations/e%201': { id: 'e 1', status: 'open', answer: null },
    'POST /api/v1/apply/escalations/e%201/resolve': { id: 'e 1', status: 'done' },
  });
  const api = createApi(settings, impl);
  const e = { link_id: 'l1', url: 'https://x', title: 'Dev', company: 'Acme', platform: 'lever', person: 'asha', kind: 'captcha', reason: 'check' };
  assert.deepEqual(await api.escalate(e), { id: 'e 1', telegram: true });
  assert.deepEqual(calls[0].body, e);
  assert.equal((await api.escalation('e 1')).status, 'open');
  await api.resolveEscalation('e 1', 'done');
  assert.deepEqual(calls[2].body, { status: 'done' });
  assert.equal(calls[2].init.headers['x-emailhelper-token'], 'agent-token');
});

test('events go to the agent in one batch', async () => {
  const { impl, calls } = fakeFetch({ 'POST /api/v1/apply/events': { stored: 2 } });
  const events = [{ kind: 'applied', link_id: 'l1' }, { kind: 'fill_failed', field: 'City' }];
  assert.deepEqual(await createApi(settings, impl).events(events), { stored: 2 });
  assert.deepEqual(calls[0].body, { events });
});

test('the knowledge service can have its own token, and resume names are encoded', async () => {
  const { impl, calls } = fakeFetch({
    'POST /v1/people/asha/resume': { file: 'my cv #1.pdf', stack: null, download: '/v1/people/asha/resumes/my cv #1.pdf' },
    'GET /v1/people/asha/resumes/my%20cv%20%231.pdf': () => new Response(new Uint8Array([1]), { status: 200 }),
  });
  const r = await createApi({ ...settings, knowledgeToken: 'kn-token' }, impl).resume('asha', []);
  assert.equal(r.file, 'my cv #1.pdf');
  assert.equal(calls[0].init.headers['x-knowledge-token'], 'kn-token');
  assert.equal(calls[1].url, 'http://127.0.0.1:7820/v1/people/asha/resumes/my%20cv%20%231.pdf');
});

test('defaults: auto-submit is off', () => {
  assert.equal(DEFAULTS.autoSubmit, false);
  assert.equal(DEFAULTS.knowledgeToken, '');
});
