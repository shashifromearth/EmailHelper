// Job applier: takes jobs one by one from the job-links queue, opens each,
// fills the application form with the agent's answers and the resume the
// knowledge service picks.
//
// By default the person submits (they press "Applied", "Skip" or "Failed" in
// the popup when done). With `autoSubmit` on, it submits by itself on
// employer career sites when every required field is filled and nothing
// needs the person; when stuck it asks the person on Telegram (via the agent)
// and checks back with exponential backoff.
//
// Hard rules: never submits on job boards (see JOB_BOARDS), never types
// passwords, never touches CAPTCHAs or verification codes, and does nothing
// to evade bot detection.

import { ApiError, DEFAULTS, createApi } from './src/api.js';
import { answerText, bestFrame, BLOCKER_TEXT, blockerFrom, boardForHost, buildPlan, canAutoSubmit, claimPlatforms, confirmedText, eventFor, jobFromLink, nextCheckAt, pausedPlatforms, publicField, recordOutcome, requiredLeft, shouldGiveUp, stuckReason, submitOutcome, toBase64 } from './src/plan.js';
import { discoverFields, markSubmitButton, readFieldStates } from './src/page/discover.js';
import { probeBlockers } from './src/page/probe.js';
import { attachFile, clickApply, clickSubmit, fillFields, highlight, readOutcome, watchSubmit } from './src/page/fill.js';

const MAX_STEPS = 5; // pages of a multi-step form submitted in one job
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const settings = async () => ({ ...DEFAULTS, ...(await chrome.storage.local.get(Object.keys(DEFAULTS))) });
const api = async () => createApi(await settings());
const run = (target, func, args = []) => chrome.scripting.executeScript({ target, func, args });

async function clientId() {
  let { clientId: id } = await chrome.storage.local.get('clientId');
  if (!id) {
    id = `chrome-${crypto.randomUUID()}`;
    await chrome.storage.local.set({ clientId: id });
  }
  return id;
}

/** One state-changing handler at a time (clicks, tab events, alarms). */
let chain = Promise.resolve();
const serial = (fn) => {
  const p = chain.then(fn);
  chain = p.catch((e) => console.warn('handler failed', e));
  return p;
};

// ---- state (session storage: survives the service worker being stopped) ----

const getState = async () => (await chrome.storage.session.get('state')).state ?? { running: false, current: null, message: '' };

async function setState(patch) {
  const s = { ...(await getState()), ...patch };
  await chrome.storage.session.set({ state: s });
  const phase = s.current?.phase;
  const badge = { filling: '…', submitting: '…', review: 'GO', blocked: '!', escalated: 'TG', error: 'ERR' }[phase] ?? '';
  chrome.action.setBadgeText({ text: badge });
  chrome.action.setBadgeBackgroundColor({ color: phase === 'review' ? '#1a9e3f' : '#b8730a' });
  chrome.runtime.sendMessage({ type: 'state', state: s }).catch(() => {});
  return s;
}

/** Patch the current job, only if it is still the one in `tabId` (a closed or finished job is left alone). */
async function setCurrent(patch, tabId) {
  const { current: cur } = await getState();
  if (!cur || cur.tabId !== tabId) return null;
  return setState({ current: { ...cur, ...patch } });
}

const isCurrent = async (tabId) => (await getState()).current?.tabId === tabId;

// ---- feedback: events and the circuit breaker ----

/** Send events to the agent; never breaks the flow. */
async function report(events) {
  if (!events.length) return;
  try {
    await (await api()).events(events);
  } catch (e) {
    console.warn('events not sent', e);
  }
}

async function breaker(platform, ok) {
  if (!platform) return;
  const { breaker: b = {} } = await chrome.storage.local.get('breaker');
  await chrome.storage.local.set({ breaker: recordOutcome(b, platform, ok, Date.now()) });
}

const paused = async () => pausedPlatforms((await chrome.storage.local.get('breaker')).breaker, Date.now());

// ---- queue ----

async function next() {
  const s = await getState();
  if (!s.running || s.current) return;
  const conf = await settings();
  const pausedNow = await paused();
  const platforms = claimPlatforms({ platforms: conf.platforms, autoSubmit: conf.autoSubmit, paused: Object.keys(pausedNow) });
  if (platforms && !platforms.length) {
    const until = Math.min(...Object.values(pausedNow));
    chrome.alarms.create('claim', { when: until });
    return setState({ message: `Every platform is paused after repeated failures; trying again at ${new Date(until).toLocaleTimeString()}.` });
  }
  let link;
  try {
    link = await (await api()).claim(await clientId(), platforms);
  } catch (e) {
    console.warn('claim failed', e);
    return setState({ running: false, message: e.message });
  }
  if (!link) return setState({ running: false, message: 'No jobs waiting in the queue.' });
  // Handlers run one at a time, so the tab's "complete" event is handled after the state below is written.
  const tab = await chrome.tabs.create({ url: link.url, active: true });
  const person = link.person_id || conf.person || null;
  await setState({ message: '', current: { link, tabId: tab.id, person, phase: 'opening', hops: 0, steps: 0 } });
}

/** Send what the person answered (captured on submit) to the agent's memory. */
async function remember(cur) {
  const answers = (cur.asks || [])
    .map((a) => ({ label: a.label, value: answerText(cur.answered?.[a.id], a.options) }))
    .filter((a) => a.value);
  if (!answers.length) return;
  try {
    await (await api()).remember(cur.person, answers);
  } catch (e) {
    console.warn('answers not remembered', e);
  }
}

let finishing = false;

/** Record the outcome, close the tab and open the next job. `tabId` guards against finishing a newer job. */
async function finish(status, reason, tabId) {
  if (finishing) return;
  finishing = true;
  let cur;
  try {
    ({ current: cur } = await getState());
    if (!cur || (tabId !== undefined && cur.tabId !== tabId)) return;
    if (status === 'applied') await remember(cur);
    await closeEscalation(cur, status === 'applied' ? 'done' : 'skip');
    let message = '';
    if (cur.link) {
      try {
        await (await api()).result(cur.link.id, await clientId(), status, reason);
      } catch (e) {
        console.warn('result not saved', e);
        // 404: the link is gone; 409: the lease expired or the job is already closed. Retry only the rest.
        if (!(e instanceof ApiError && (e.status === 404 || e.status === 409))) return setState({ message: e.message });
        message = `Job-links did not take the result (${e.message}); moved on.`;
      }
      const kind = { applied: 'applied', skipped: 'skipped' }[status] || 'error';
      await report([eventFor(cur, kind, { reason: reason || null })]);
      if (status === 'applied') await breaker(cur.link.platform, true);
      else if (status === 'failed' && !cur.countedFailure) await breaker(cur.link.platform, false);
    }
    // Clear first, so closing the tab does not look like the person closing it.
    await setState({ current: null, message });
    if (status !== 'failed' && cur.tabId != null) {
      try {
        await chrome.tabs.remove(cur.tabId);
      } catch {
        /* already closed */
      }
    }
  } finally {
    finishing = false;
  }
  await next();
}

// ---- filling ----

/**
 * Fill the form in `tabId`. `manual`: the person pressed "Fill this page",
 * so the current job follows them to that tab.
 */
async function fillTab(tabId, manual = false) {
  const s = await getState();
  let cur = s.current;
  if (cur && cur.tabId !== tabId) {
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!manual && tab?.openerTabId !== cur.tabId) {
      return setState({ message: 'Finish the current job first (Applied, Skip or Failed).' });
    }
    cur = { ...cur, tabId }; // the form opened in another tab: the job moves there
  }
  if (cur?.phase === 'filling' || cur?.phase === 'submitting') return;
  if (cur?.phase === 'escalated') {
    await closeEscalation(s.current, 'done'); // the person is here: stop waiting for Telegram
    cur = { ...(await getState()).current, tabId };
  }
  cur ??= { link: null, tabId, person: (await settings()).person || null, hops: 0, steps: 0 };
  await setState({ message: '', current: { ...cur, phase: 'filling', blocker: null, status: 'Reading the form…' } });
  try {
    // State is written before navigating, so the tab's "complete" finds phase 'opening'.
    const { navigate, ...patch } = await fillPage(cur);
    await setCurrent(patch, tabId);
    if (navigate) await chrome.tabs.update(tabId, { url: navigate });
  } catch (e) {
    console.warn('fill failed', e);
    if (!(await isCurrent(tabId))) return;
    await report([eventFor(cur, 'error', { reason: e.message })]);
    await setCurrent({ phase: 'error', blocker: null, status: e.message }, tabId);
  }
  await afterFill(tabId);
}

/** Discover until the form stops changing: many job sites draw it after the page loads. */
async function discoverSettled(tabId) {
  let found = [];
  let last = -1;
  for (let i = 0; i < 8; i++) {
    found = await run({ tabId, allFrames: true }, discoverFields);
    const n = bestFrame(found)?.result.fields.length ?? 0;
    if (n > 0 && n === last) break;
    last = n;
    await sleep(700);
  }
  return found;
}

/** The form's fields and anything only the person can clear. */
async function inspect(tabId) {
  const found = await discoverSettled(tabId);
  const best = bestFrame(found);
  const fields = best?.result.appLike ? best.result.fields : [];
  const frameIds = [...new Set([0, best?.frameId ?? 0])];
  const probes = (await run({ tabId, frameIds }, probeBlockers)).map((r) => r.result).filter(Boolean);
  // a signal seen in either frame counts
  const probe = Object.assign({}, ...probes.map((p) => Object.fromEntries(Object.entries(p).filter(([, v]) => v))));
  const url = (await chrome.tabs.get(tabId)).url;
  return { found, best, fields, url, blocker: blockerFrom(probe, url, fields) };
}

const readOutcomes = async (tabId) => (await run({ tabId, allFrames: true }, readOutcome)).map((r) => r.result).filter(Boolean);

async function fillPage(cur) {
  const { found, best, fields, url, blocker } = await inspect(cur.tabId);
  cur = { ...cur, url };
  if (blocker) await report([eventFor(cur, 'blocked', { reason: blocker.kind, detail: blocker.detail })]);

  if (!fields.length) {
    if (blocker) return { url, blocker, phase: 'blocked', status: `${BLOCKER_TEXT[blocker.kind]}, then press “Fill this page”.` };
    const apply = found.find((r) => r.result?.apply);
    if (apply && cur.hops < 2) {
      const hop = { url, blocker: null, phase: 'opening', hops: cur.hops + 1, status: 'Opening the application form…' };
      if (apply.result.apply.href) return { ...hop, navigate: apply.result.apply.href };
      const before = new Set((await chrome.tabs.query({})).map((t) => t.id));
      await run({ tabId: cur.tabId, frameIds: [apply.frameId] }, clickApply);
      await sleep(2500);
      const opened = (await chrome.tabs.query({})).find((t) => t.openerTabId === cur.tabId && !before.has(t.id));
      if (opened) return { ...hop, tabId: opened.id }; // filled when that tab finishes loading
      return fillPage({ ...cur, hops: cur.hops + 1 });
    }
    return { url, blocker: null, phase: 'waiting', status: 'No application form here yet. Open it, then press “Fill this page”.' };
  }

  const frame = { tabId: cur.tabId, frameIds: [best.frameId] };
  const services = await api();
  const { person, answers } = await services.answers(cur.person, jobFromLink(cur.link), fields.map(publicField));
  const plan = buildPlan(fields, answers);
  const [{ result: done }] = await run(frame, fillFields, [plan.steps]);
  const problems = [...plan.problems, ...done.failed];

  let resume = null;
  if (plan.resume.length) {
    try {
      const r = await services.resume(person, cur.link?.skills);
      const b64 = toBase64(r.bytes);
      for (const id of plan.resume) {
        const [{ result: err }] = await run(frame, attachFile, [id, r.file, r.type, b64]);
        if (err) problems.push({ id, reason: err });
      }
      resume = r.stack ? `${r.file} (${r.stack})` : r.file;
    } catch (e) {
      problems.push({ id: plan.resume[0], reason: `resume: ${e.message}` });
    }
  }

  const field = (id) => fields.find((f) => f.id === id);
  const label = (id) => field(id)?.label || id;
  await report(problems.map((p) => eventFor({ ...cur, person }, 'fill_failed', { field: label(p.id), reason: p.reason })));
  let asks = [
    ...plan.asks.map((a) => ({ ...a, problem: false })),
    ...problems.map((p) => ({ id: p.id, label: label(p.id), reason: p.reason, problem: true })),
  ];
  const asked = new Set(asks.map((a) => a.id));
  const check = fields.filter((f) => f.required && !asked.has(f.id)).map((f) => f.id);
  if (check.length) {
    const [{ result: states }] = await run(frame, readFieldStates, [check]);
    asks.push(...requiredLeft(fields, states || {}, asked));
  }
  asks = asks.map((a) => ({ ...a, required: !!field(a.id)?.required, options: field(a.id)?.options || [] }));

  const [{ result: submit }] = await run(frame, markSubmitButton);
  await run(frame, highlight, [asks]);
  await run(frame, watchSubmit, [asks.map((a) => a.id)]);
  const todo = blocker ? `${BLOCKER_TEXT[blocker.kind]}. ` : '';
  return {
    url,
    phase: 'review',
    person,
    resume,
    blocker,
    frameId: best.frameId,
    submitFound: !!submit,
    thanksBefore: confirmedText(await readOutcomes(cur.tabId)),
    filled: done.filled.length + (resume ? plan.resume.length : 0),
    asks,
    status: `${todo}Review the form, submit it on the page, then press “Applied”.`,
  };
}

// ---- autonomy: submit, or ask on Telegram ----

/** With auto-submit on: submit when allowed, otherwise ask the person if stuck. Off: the person drives. */
async function afterFill(tabId) {
  const { current: cur } = await getState();
  if (!cur || cur.tabId !== tabId || !cur.link) return;
  const s = await settings();
  if (!s.autoSubmit) return;
  // the page actually open counts: an employer link can redirect to a job board
  let host = '';
  try {
    host = new URL((await chrome.tabs.get(tabId)).url).hostname;
  } catch {
    return;
  }
  const facts = {
    platform: boardForHost(host) || cur.link.platform,
    autoSubmit: true,
    blocker: cur.blocker,
    submitFound: cur.submitFound,
    asks: (cur.asks || []).filter((a) => !a.problem),
    problems: (cur.asks || []).filter((a) => a.problem),
  };
  if (cur.phase === 'review' && canAutoSubmit(facts)) {
    try {
      return await submitAndConfirm(cur);
    } catch (e) {
      console.warn('submit failed', e);
      if (!(await isCurrent(tabId))) return;
      await report([eventFor(cur, 'submit_failed', { reason: 'error while submitting', detail: e.message })]);
      await setCurrent({ phase: 'review' }, tabId);
      return escalate(cur, 'submit_failed', `Error while submitting (${e.message}); check whether it went through`);
    }
  }
  const stuck = stuckReason({ ...facts, phase: cur.phase, status: cur.status });
  if (stuck) await escalate(cur, stuck.kind, stuck.reason);
}

/** Wait for the page to settle after a click: a finished navigation, or ~6 s. */
function settle(tabId, ms = 6000) {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      chrome.tabs.onUpdated.removeListener(onUpdated);
      setTimeout(resolve, 1000);
    };
    const onUpdated = (id, info) => id === tabId && info.status === 'complete' && done();
    const timer = setTimeout(done, ms);
    chrome.tabs.onUpdated.addListener(onUpdated);
  });
}

async function submitAndConfirm(cur) {
  const { tabId } = cur;
  await setCurrent({ phase: 'submitting', status: 'Submitting the application…' }, tabId);
  const frame = { tabId, frameIds: [cur.frameId ?? 0] };
  const before = await readOutcomes(tabId);
  const [{ result: clicked }] = await run(frame, clickSubmit);
  if (!clicked) {
    await setCurrent({ phase: 'review' }, tabId);
    return escalate(cur, 'no_submit', 'The submit button disappeared');
  }
  await settle(tabId);
  if (!(await isCurrent(tabId))) return;
  const { result, errors } = submitOutcome(before, await readOutcomes(tabId));
  if (result === 'confirmed') return finish('applied', null, tabId);
  if (result === 'next_step' && (cur.steps || 0) < MAX_STEPS) {
    await setCurrent({ phase: 'opening', steps: (cur.steps || 0) + 1, hops: 0, status: 'Next page of the form…' }, tabId);
    return fillTab(tabId);
  }
  const detail = errors.join('; ') || null;
  await report([eventFor(cur, 'submit_failed', { reason: 'no confirmation after submit', detail })]);
  await breaker(cur.link?.platform, false);
  await setCurrent({ phase: 'review', countedFailure: true }, tabId);
  return escalate(cur, 'submit_failed', detail ? `The site did not accept the form: ${detail}`.slice(0, 500) : 'The site did not confirm the application');
}

function schedule(esc) {
  const nextAt = nextCheckAt(esc, Date.now());
  chrome.alarms.create('escalation', { when: nextAt });
  return { ...esc, nextAt };
}

/** Ask the person on Telegram and wait (phase `escalated`). Keeps the first start time, so the 2 h limit is per job. */
async function escalate(cur, kind, reason) {
  const link = cur.link || {};
  let r;
  try {
    r = await (await api()).escalate({
      link_id: link.id ?? null,
      url: link.url ?? cur.url ?? null,
      title: link.title ?? null,
      company: link.company ?? null,
      platform: link.platform ?? null,
      person: cur.person ?? null,
      kind,
      reason,
    });
  } catch (e) {
    console.warn('escalation failed', e);
    return setCurrent({ status: `${reason}. Could not ask on Telegram (${e.message}); do it here, then press a button.` }, cur.tabId);
  }
  await report([eventFor(cur, 'escalated', { reason: kind, detail: reason })]);
  const prev = cur.escalation;
  const esc = schedule({
    id: r.id,
    kind,
    reason,
    telegram: r.telegram,
    attempt: prev ? prev.attempt + 1 : 0,
    startedAt: prev?.startedAt ?? Date.now(),
  });
  return setCurrent({ phase: 'escalated', escalation: esc, status: reason }, cur.tabId);
}

async function closeEscalation(cur, status) {
  const esc = cur?.escalation;
  if (!esc || esc.resolved) return;
  await chrome.alarms.clear('escalation');
  try {
    await (await api()).resolveEscalation(esc.id, status);
  } catch (e) {
    console.warn('escalation not resolved', e);
  }
  await setCurrent({ escalation: { ...esc, resolved: true } }, cur.tabId);
}

const BLOCKER_KINDS = Object.keys(BLOCKER_TEXT);
const FORM_KINDS = ['questions', 'submit_failed', 'no_submit'];

/** One backoff tick while waiting for the person. */
async function tick() {
  const { current: cur } = await getState();
  if (cur?.phase !== 'escalated' || !cur.escalation) return;
  const esc = cur.escalation;
  if (shouldGiveUp(esc, Date.now())) {
    await closeEscalation(cur, 'skip');
    return finish('failed', 'no human response', cur.tabId);
  }
  let status = 'open';
  try {
    status = (await (await api()).escalation(esc.id)).status;
  } catch (e) {
    console.warn('escalation not checked', e);
  }
  if (status === 'skip') {
    await closeEscalation(cur, 'skip');
    return finish('skipped', 'skipped on Telegram', cur.tabId);
  }
  let moved = false;
  try {
    moved = await recheck(cur, status === 'done');
  } catch (e) {
    console.warn('recheck failed', e); // the tab may be gone or mid-navigation: check again later
  }
  if (moved || !(await isCurrent(cur.tabId))) return;
  await setCurrent({ escalation: schedule({ ...esc, attempt: esc.attempt + 1 }) }, cur.tabId);
}

/**
 * Has the person fixed it? Returns true when the job moved on. Without a
 * "done" reply, only cheap checks run: a confirmation on the page, the
 * required fields now filled, or the blocker gone. Errors and missing forms
 * wait for "done", so a broken page does not re-ask on every tick.
 */
async function recheck(cur, saidDone) {
  const esc = cur.escalation;
  if (FORM_KINDS.includes(esc.kind)) {
    if (confirmedText(await readOutcomes(cur.tabId)) && !cur.thanksBefore) {
      await closeEscalation(cur, 'done');
      await finish('applied', null, cur.tabId);
      return true;
    }
    if (esc.kind !== 'questions' && !saidDone) return false; // never re-submit unasked
    const blocking = (cur.asks || []).filter((a) => a.required || a.problem);
    let left = blocking;
    if (blocking.length) {
      const [{ result: states }] = await run({ tabId: cur.tabId, frameIds: [cur.frameId ?? 0] }, readFieldStates, [blocking.map((a) => a.id)]);
      left = blocking.filter((a) => states?.[a.id]?.empty !== false);
    }
    if (left.length && !saidDone) return false;
    await closeEscalation(cur, 'done');
    if (left.length) {
      await escalate(cur, 'questions', `Still empty: ${left.map((a) => a.label).join(', ')}`.slice(0, 500));
      return true;
    }
    const [{ result: submit }] = await run({ tabId: cur.tabId, frameIds: [cur.frameId ?? 0] }, markSubmitButton);
    const asks = cur.asks.map((a) => ({ ...a, required: false, problem: false }));
    await setCurrent({ phase: 'review', asks, submitFound: !!submit, status: 'Fixed by the person.' }, cur.tabId);
    await afterFill(cur.tabId);
    return true;
  }
  if (!saidDone) {
    if (!BLOCKER_KINDS.includes(esc.kind)) return false;
    const page = await inspect(cur.tabId);
    if (page.blocker || !page.fields.length) return false;
  }
  await closeEscalation(cur, 'done');
  await fillTab(cur.tabId);
  return true;
}

// ---- events ----

chrome.tabs.onUpdated.addListener((tabId, info) => {
  if (info.status !== 'complete') return;
  serial(async () => {
    const { current: cur } = await getState();
    if (cur?.tabId === tabId && cur.phase === 'opening') await fillTab(tabId);
  });
});

// An Apply button that opens the form in a new tab: the job follows it.
chrome.tabs.onCreated.addListener((tab) =>
  serial(async () => {
    const { current: cur } = await getState();
    if (!cur || tab.openerTabId !== cur.tabId || !['opening', 'waiting'].includes(cur.phase)) return;
    await setCurrent({ tabId: tab.id, phase: 'opening', status: 'Opening the application form…' }, cur.tabId);
  }),
);

chrome.tabs.onRemoved.addListener((tabId) =>
  serial(async () => {
    const { current: cur } = await getState();
    if (cur?.tabId !== tabId) return;
    await closeEscalation(cur, 'skip');
    // The claim expires on its own and the job goes back to the queue.
    await setState({
      running: false,
      current: null,
      message: cur.link ? 'Tab closed: paused. The job returns to the queue when its claim expires.' : '',
    });
  }),
);

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === 'escalation') serial(tick);
  if (alarm.name === 'claim') serial(next);
});

chrome.commands.onCommand.addListener((command) => {
  if (command !== 'fill-page') return;
  serial(async () => {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (tab) await fillTab(tab.id, true);
  });
});

async function handle(msg) {
  switch (msg.type) {
    case 'get': {
      let stats = null;
      try {
        stats = await (await api()).stats();
      } catch {
        /* shown as unreachable */
      }
      return { state: await getState(), stats, autoSubmit: (await settings()).autoSubmit, paused: await paused() };
    }
    case 'start':
      await setState({ running: true, message: '' });
      await next();
      break;
    case 'pause':
      await setState({ running: false });
      break;
    case 'fill': {
      const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
      if (tab) await fillTab(tab.id, true);
      break;
    }
    case 'finish':
      await finish(msg.status, msg.reason);
      break;
    default:
      throw new Error(`unknown message ${msg.type}`);
  }
  return { state: await getState() };
}

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  if (msg?.type === 'state') return false;
  if (msg?.type === 'answered') {
    getState().then(({ current: cur }) => {
      if (cur && sender.tab?.id === cur.tabId) setCurrent({ answered: { ...cur.answered, ...msg.values } }, cur.tabId);
    });
    return false;
  }
  // Reads do not wait in line; neither does Skip/Failed during a long fill (the fill's later writes are tab-guarded).
  (async () => {
    const filling = (await getState()).current?.phase === 'filling';
    return msg?.type === 'get' || (msg?.type === 'finish' && filling) ? handle(msg) : serial(() => handle(msg));
  })().then(reply, (e) => reply({ error: e?.message }));
  return true;
});

// A fresh service worker cannot be mid-fill: a stored 'filling' is left over from a worker MV3 stopped.
serial(async () => {
  const { current: cur } = await getState();
  if (cur?.phase === 'filling') {
    await setCurrent({ phase: 'error', blocker: null, status: 'Stopped while filling. Press “Fill this page” to try again.' }, cur.tabId);
    await afterFill(cur.tabId);
  } else if (cur?.phase === 'submitting') {
    await setCurrent({ phase: 'review', status: 'Stopped while submitting: check whether it went through.' }, cur.tabId);
    if ((await settings()).autoSubmit && cur.link) await escalate(cur, 'submit_failed', 'Stopped while submitting; check whether it went through');
  }
});
