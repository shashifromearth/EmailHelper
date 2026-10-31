const $ = (id) => document.getElementById(id);
const send = (msg) => chrome.runtime.sendMessage(msg);
const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

let escalation = null;

function renderWaiting() {
  $('waiting').hidden = !escalation;
  if (!escalation) return;
  const secs = Math.max(0, Math.round((escalation.nextAt - Date.now()) / 1000));
  const asked = escalation.telegram === false ? 'Telegram is not set up' : 'asked on Telegram';
  $('waiting').textContent = `Waiting for you — ${asked}; next check in ${secs}s. Fix it on the page or reply skip.`;
}

function render({ state, stats, autoSubmit, paused, error } = {}) {
  if (stats !== undefined) {
    $('queue').textContent = stats ? `queue: ${stats.by_status?.new ?? 0} new` : 'queue: offline';
    $('queue').className = `pill ${stats ? 'ok' : 'err'}`;
  }
  if (autoSubmit !== undefined) {
    $('mode').textContent = autoSubmit
      ? 'Auto-submit on (employer sites only); passwords and checks stay with you.'
      : 'You always submit; passwords and checks stay with you.';
  }
  if (paused !== undefined) {
    const list = Object.entries(paused || {}).map(([p, until]) => `${p} until ${time(until)}`);
    $('paused').hidden = !list.length;
    $('paused').textContent = list.length ? `Paused after repeated failures: ${list.join(', ')}` : '';
  }
  const msg = error || state?.message;
  $('message').hidden = !msg;
  $('message').textContent = msg || '';
  if (!state) return;
  $('start').hidden = state.running;
  $('pause').hidden = !state.running;

  const cur = state.current;
  $('job').hidden = !cur;
  escalation = cur?.phase === 'escalated' ? cur.escalation : null;
  renderWaiting();
  if (!cur) return;
  const link = cur.link || {};
  $('title').textContent = link.title || (cur.link ? link.url : 'This page');
  $('meta').textContent = [link.company, link.platform, link.remote !== 'unknown' && link.remote, link.source, link.score != null && `score ${link.score}`]
    .filter(Boolean)
    .join(' · ');
  $('status').textContent = cur.status || '';
  $('details').textContent = [cur.filled != null && `${cur.filled} fields filled`, cur.resume && `resume ${cur.resume}`, cur.person && `as ${cur.person}`]
    .filter(Boolean)
    .join(' · ');
  $('asks').replaceChildren(
    ...(cur.asks || []).map((a) => {
      const li = document.createElement('li');
      li.textContent = a.reason ? `${a.label}: ${a.reason}` : a.label;
      return li;
    }),
  );
  // Skip and Failed always work, so a stuck fill can be left.
  const busy = cur.phase === 'filling' || cur.phase === 'submitting';
  $('applied').disabled = busy;
  $('fill').disabled = busy;
}

const act = (msg) => send(msg).then(render);
$('start').onclick = () => act({ type: 'start' });
$('pause').onclick = () => act({ type: 'pause' });
$('fill').onclick = () => act({ type: 'fill' });
$('applied').onclick = () => act({ type: 'finish', status: 'applied' });
const reason = (fallback) => $('reason').value.trim() || fallback;
$('skip').onclick = () => act({ type: 'finish', status: 'skipped', reason: reason('skipped by the person') });
$('failed').onclick = () => act({ type: 'finish', status: 'failed', reason: reason('failed') });
$('options').onclick = (e) => {
  e.preventDefault();
  chrome.runtime.openOptionsPage();
};

chrome.runtime.onMessage.addListener((m) => m?.type === 'state' && render({ state: m.state }));
send({ type: 'get' }).then(render);
setInterval(renderWaiting, 1000);
