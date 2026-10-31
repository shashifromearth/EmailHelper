// EmailHelper background script (loaded as an ES module from background.html).
//
// Wires the pure modules in src/ to Thunderbird:
//   new mail  -> ingest -> outbox (storage.local) -> POST /api/v1/emails
//   long poll -> GET /api/v1/drafts/pending -> compose + save draft -> ack
//
// This extension NEVER sends mail. It only saves drafts; the user reviews and
// sends them manually.

import { CONFIG_KEY, loadConfig } from "./src/config.js";
import { createApiClient, isAuthError } from "./src/api.js";
import { DedupeStore } from "./src/dedupe.js";
import { Outbox } from "./src/outbox.js";
import { DraftPoller, DraftProcessor } from "./src/drafts.js";
import { createIngestor } from "./src/ingest.js";
import { StateStore, computeBadge } from "./src/status.js";

const mx = globalThis.messenger || globalThis.browser;
const storage = mx.storage.local;
const OUTBOX_RETRY_MS = 60 * 1000;

function log(level, message) {
  const fn = level === "error" ? console.error : level === "warn" ? console.warn : console.log;
  fn(`[EmailHelper] ${message}`);
}

// ---------------------------------------------------------------------------
// Config (cached, refreshed on storage change)
// ---------------------------------------------------------------------------

let configPromise = null;
function getConfig() {
  if (!configPromise) configPromise = loadConfig(storage).catch((e) => ((configPromise = null), Promise.reject(e)));
  return configPromise;
}

// ---------------------------------------------------------------------------
// State + badge
// ---------------------------------------------------------------------------

async function applyBadge(state) {
  const badge = computeBadge(state);
  try {
    await mx.browserAction.setBadgeText({ text: badge.text });
    await mx.browserAction.setBadgeBackgroundColor({ color: badge.color });
    await mx.browserAction.setTitle({ title: badge.title });
  } catch (err) {
    log("warn", `badge update failed: ${err.message}`);
  }
}

const state = new StateStore({ storage, onChange: applyBadge });
const api = createApiClient({ getConfig });

// ---------------------------------------------------------------------------
// Outbox + dedupe + ingestion
// ---------------------------------------------------------------------------

const dedupe = new DedupeStore({ storage });
const outbox = new Outbox({
  storage,
  send: (payload) => api.submitEmail(payload),
  log,
  onChange: (depth) => state.update({ outboxDepth: depth }),
});

let outboxTimer = null;
function scheduleOutboxFlush(delayMs) {
  if (outboxTimer) clearTimeout(outboxTimer);
  outboxTimer = setTimeout(() => {
    outboxTimer = null;
    flushOutbox();
  }, delayMs);
}

let outboxFlushing = null;
function flushOutbox() {
  if (!outboxFlushing) {
    outboxFlushing = doFlushOutbox()
      .catch((err) => log("error", `outbox flush failed: ${err.message}`))
      .finally(() => {
        outboxFlushing = null;
      });
  }
  return outboxFlushing;
}

async function doFlushOutbox() {
  if (!outbox.size) return;
  const cfg = await getConfig();
  if (!cfg.apiToken) return; // flushed again once a token is saved
  const r = await outbox.flush();
  const patch = (s) => ({
    emailsSubmitted: s.emailsSubmitted + r.delivered,
    emailsDuplicate: s.emailsDuplicate + r.duplicates,
    emailsDropped: s.emailsDropped + r.dropped,
  });
  await state.update(patch);
  if (r.stopped) {
    await state.update({ backendReachable: r.stopped === "auth" ? true : false, authError: r.stopped === "auth" });
    if (r.lastError) await state.recordError(`email delivery: ${r.lastError.message}`);
  } else if (r.delivered || r.duplicates) {
    await state.update({ backendReachable: true, authError: false });
  }
  if (outbox.size) scheduleOutboxFlush(OUTBOX_RETRY_MS);
}

// Senders the user excluded in the agent's .env (EH_EXCLUDED_SENDERS),
// refreshed from /api/v1/status at most every 10 minutes.
let excludedSenders = { at: 0, list: [] };
async function getExcludedSenders() {
  if (Date.now() - excludedSenders.at < 10 * 60 * 1000) return excludedSenders.list;
  try {
    const s = await api.status();
    excludedSenders = { at: Date.now(), list: Array.isArray(s?.excluded_senders) ? s.excluded_senders : [] };
  } catch {
    // keep the last list; the agent drops excluded senders again on its side
    excludedSenders.at = Date.now() - 9 * 60 * 1000;
  }
  return excludedSenders.list;
}

const ingestor = createIngestor({
  messenger: mx,
  getConfig,
  dedupe,
  outbox,
  log,
  getExcludedSenders,
  onEvent: (ev) => {
    if (ev.type === "error") state.recordError(`reading message: ${ev.error.message}`);
    if (ev.type === "invalid") state.recordError(`message ${ev.tbMessageId} skipped: ${ev.problems.join("; ")}`);
  },
});

// ---------------------------------------------------------------------------
// Drafts
// ---------------------------------------------------------------------------

async function notifyDraft(draft) {
  const cfg = await getConfig();
  if (!cfg.notifyOnDraft || !mx.notifications) return;
  const opts = {
    type: "basic",
    title: "Draft ready",
    message: (draft.validation === "REVIEW_REQUIRED" ? "[Review required] " : "") + (draft.subject || "(no subject)"),
    iconUrl: mx.runtime.getURL("icons/icon.svg"),
  };
  try {
    await mx.notifications.create(`eh-draft-${draft.draft_id}`, opts);
  } catch {
    delete opts.iconUrl; // some platforms reject SVG notification icons
    await mx.notifications.create(`eh-draft-${draft.draft_id}`, opts);
  }
}

const processor = new DraftProcessor({
  api,
  messenger: mx,
  storage,
  log,
  notify: notifyDraft,
  onEvent: (ev) => {
    if (ev.type === "draft_saved") {
      state.update((s) => ({ draftsSaved: s.draftsSaved + 1, unseenDrafts: s.unseenDrafts + 1, lastDraftAt: Date.now() }));
    } else if (ev.type === "draft_failed") {
      state.update((s) => ({ draftsFailed: s.draftsFailed + 1, unseenFailures: s.unseenFailures + 1 }));
      state.recordError(`draft ${ev.draftId}: ${ev.error}`);
    }
  },
});

const poller = new DraftPoller({
  api,
  processor,
  getConfig,
  log,
  onStatus: (ev) => {
    if (ev.type === "ok") {
      state.update({ backendReachable: true, authError: false, notConfigured: false, lastPollOkAt: Date.now() });
      if (ev.recovered) flushOutbox(); // backend is back: deliver queued mail now
    } else if (ev.type === "error") {
      const auth = isAuthError(ev.error);
      state.update({ backendReachable: auth ? true : false, authError: auth, notConfigured: false });
      state.recordError(`long poll: ${ev.error.message}`);
    } else if (ev.type === "not_configured") {
      if (!state.get().notConfigured) state.update({ notConfigured: true });
    }
  },
});

// ---------------------------------------------------------------------------
// New-mail listener (registered synchronously at top level)
// ---------------------------------------------------------------------------

async function onNewMail(folder, messageList) {
  try {
    await startup(); // make sure stores are loaded if this event woke us up
    const counts = await ingestor.handleNewMail(folder, messageList);
    if (counts.queued) flushOutbox();
  } catch (err) {
    log("error", `onNewMailReceived handler failed: ${err.message}`);
    state.recordError(`new mail: ${err.message}`);
  }
}

let monitoringAllFolders = false;
function registerNewMailListener(monitorAllFolders) {
  const ev = mx.messages.onNewMailReceived;
  if (ev.hasListener(onNewMail)) ev.removeListener(onNewMail);
  monitoringAllFolders = false;
  if (monitorAllFolders) {
    try {
      ev.addListener(onNewMail, true); // TB121+: also report mail filtered into non-inbox folders
      monitoringAllFolders = true;
      return;
    } catch {
      /* TB115 has no extra parameter */
    }
  }
  ev.addListener(onNewMail);
}
registerNewMailListener(false);

// ---------------------------------------------------------------------------
// Startup / config changes / control port
// ---------------------------------------------------------------------------

let startPromise = null;
function startup() {
  if (!startPromise) {
    startPromise = (async () => {
      await state.load(); // first: outbox.load() reports its depth into the state store
      await Promise.all([dedupe.load(), outbox.load(), processor.load()]);
      await state.update({ startedAt: Date.now(), outboxDepth: outbox.size });
      const cfg = await getConfig();
      if (cfg.monitorAllFolders) registerNewMailListener(true);
      poller.start();
      flushOutbox();
      log("info", `started; ${outbox.size} queued e-mail(s), ${processor.pendingAcks.length} pending ack(s)`);
    })().catch((err) => {
      startPromise = null;
      log("error", `startup failed: ${err.message}`);
      throw err;
    });
  }
  return startPromise;
}

mx.storage.onChanged.addListener((changes, area) => {
  if (area !== "local" || !changes[CONFIG_KEY]) return;
  configPromise = null;
  getConfig().then((cfg) => {
    if (cfg.monitorAllFolders !== monitoringAllFolders) registerNewMailListener(cfg.monitorAllFolders);
    poller.wake(); // restart the long poll against the new URL/token
    flushOutbox();
  });
});

for (const evName of ["onCreated", "onUpdated", "onDeleted"]) {
  try {
    mx.accounts?.[evName]?.addListener(() => ingestor.invalidateAccounts());
  } catch {
    /* event not available in this TB version */
  }
}

// Popup / options talk to us through a port (no one-shot runtime messages).
mx.runtime.onConnect.addListener((port) => {
  if (port.name !== "eh-control") return;
  port.onMessage.addListener(async (msg) => {
    const reply = (data) => {
      try {
        port.postMessage({ id: msg?.id, ...data });
      } catch {
        /* port closed */
      }
    };
    try {
      await startup();
      switch (msg?.cmd) {
        case "popupOpened":
          await state.update({ unseenDrafts: 0, unseenFailures: 0 });
          reply({ ok: true });
          break;
        case "processUnread": {
          const totals = await ingestor.processExistingUnread();
          flushOutbox();
          reply({ ok: true, totals });
          break;
        }
        case "flushOutbox":
          await flushOutbox();
          reply({ ok: true, outboxDepth: outbox.size });
          break;
        case "clearError":
          await state.update({ lastError: null });
          reply({ ok: true });
          break;
        default:
          reply({ ok: false, error: `unknown command ${msg?.cmd}` });
      }
    } catch (err) {
      reply({ ok: false, error: err.message });
    }
  });
});

mx.runtime.onStartup?.addListener(() => startup());
mx.runtime.onInstalled?.addListener((details) => {
  startup();
  if (details?.reason === "install") mx.runtime.openOptionsPage().catch(() => {});
});

// Persistent background page: also start immediately on load.
startup();
