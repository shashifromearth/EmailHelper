// Draft pipeline: long poll GET /api/v1/drafts/pending, open a compose window
// per draft, save it as a Thunderbird draft, close the window, ack.
//
// SAFETY: this extension only ever *saves* drafts. It never calls the compose
// API's send function, anywhere. tests/drafts.test.js scans every source file
// to enforce that.
//
// `messenger` is injected so the whole flow runs in Node against a fake.

import { stripAngleBrackets, shouldSkipFolder } from "./extract.js";
import { isAuthError, isRetryableError } from "./api.js";

export const SAVED_DRAFTS_KEY = "ehSavedDrafts";
export const PENDING_ACKS_KEY = "ehPendingAcks";
export const REVIEW_REQUIRED = "REVIEW_REQUIRED";

const defaultSleep = (ms, signal) =>
  new Promise((resolve) => {
    const t = setTimeout(resolve, ms);
    if (signal) signal.addEventListener("abort", () => (clearTimeout(t), resolve()), { once: true });
  });

const errMsg = (e) => (e && e.message ? e.message : String(e));

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/** Contract: `[EmailHelper: REVIEW REQUIRED – <notes joined by "; ">]` on top when REVIEW_REQUIRED. */
export function reviewBanner(notes) {
  const list = Array.isArray(notes) ? notes.filter((n) => typeof n === "string" && n.trim()).map((n) => n.trim()) : [];
  return `[EmailHelper: REVIEW REQUIRED – ${list.length ? list.join("; ") : "no reason given"}]`;
}

export function buildDraftBody(draft) {
  const body = typeof draft?.body === "string" ? draft.body.replace(/\r\n?/g, "\n") : "";
  if (draft?.validation !== REVIEW_REQUIRED) return body;
  return `${reviewBanner(draft.notes)}\n\n${body}`;
}

export function escapeHtml(text) {
  return String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** Plain text -> minimal HTML document for HTML-mode compose windows. */
export function textToHtml(text) {
  const inner = escapeHtml(text).replace(/\n/g, "<br>\n");
  return `<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>${inner}</body></html>`;
}

/** Serialises async jobs: at most one compose window is open at a time. */
export function createMutex() {
  let tail = Promise.resolve();
  return {
    run(fn) {
      const next = tail.then(() => fn());
      tail = next.catch(() => {});
      return next;
    },
  };
}

// ---------------------------------------------------------------------------
// messenger.* helpers
// ---------------------------------------------------------------------------

/** Default identity of an account: identities.getDefault -> accounts.getDefaultIdentity -> accounts.get().identities[0]. */
export async function resolveIdentity(messenger, accountId) {
  if (!accountId) return null;
  const attempts = [
    () => messenger.identities?.getDefault?.(accountId),
    () => messenger.accounts?.getDefaultIdentity?.(accountId),
    async () => (await messenger.accounts?.get?.(accountId, false))?.identities?.[0],
  ];
  for (const fn of attempts) {
    try {
      const identity = await fn();
      if (identity && identity.id) return identity;
    } catch {
      /* try next */
    }
  }
  return null;
}

/**
 * Thunderbird message id to reply to. Verifies `tb_message_id` still refers to
 * the same Message-ID (ids can change when mail is moved or the index is
 * rebuilt) and otherwise looks the message up by its Message-ID header.
 */
export async function resolveReplyMessageId(messenger, draft) {
  const wanted = stripAngleBrackets(draft.message_id || "");
  const tbId = Number.isInteger(draft.tb_message_id) ? draft.tb_message_id : null;

  if (tbId !== null) {
    if (typeof messenger.messages?.get !== "function") return tbId;
    try {
      const h = await messenger.messages.get(tbId);
      if (h && (!wanted || stripAngleBrackets(h.headerMessageId || "") === wanted)) return h.id ?? tbId;
    } catch {
      /* fall through to lookup */
    }
  }

  if (wanted && typeof messenger.messages?.query === "function") {
    try {
      const page = await messenger.messages.query({ headerMessageId: wanted });
      const found = (page?.messages || []).filter((m) => stripAngleBrackets(m.headerMessageId || "") === wanted);
      const usable = found.filter((m) => !m.folder || !shouldSkipFolder(m.folder));
      const sameAccount = usable.find((m) => m.folder?.accountId === draft.account_id);
      const pick = sameAccount || usable[0] || found[0];
      if (pick) return pick.id;
    } catch {
      /* not supported or failed */
    }
  }
  throw new Error(`original message not found (tb_message_id=${draft.tb_message_id}, message_id=${draft.message_id || "null"})`);
}

async function closeComposeTab(messenger, tab) {
  try {
    if (tab.windowId !== undefined && messenger.windows?.remove) {
      await messenger.windows.remove(tab.windowId);
      return;
    }
  } catch {
    /* fall back to closing the tab */
  }
  try {
    await messenger.tabs?.remove?.(tab.id);
  } catch {
    /* already closed */
  }
}

/**
 * Open a compose window for `draft`, save it as a draft and close it.
 * Returns { tbDraftMessageId } (null when Thunderbird does not report it).
 */
export async function saveDraft(draft, { messenger, sleep = defaultSleep, saveRetryDelayMs = 750 } = {}) {
  if (draft.kind !== "reply" && draft.kind !== "new") throw new Error(`unsupported draft kind: ${draft.kind}`);

  const text = buildDraftBody(draft);
  const identity = await resolveIdentity(messenger, draft.account_id);
  // Thunderbird composes HTML unless the identity is set to plain text.
  const composeHtml = identity ? identity.composeHtml !== false : true;
  const details = composeHtml ? { isPlainText: false, body: textToHtml(text) } : { isPlainText: true, plainTextBody: text };

  let tab;
  if (draft.kind === "reply") {
    const messageId = await resolveReplyMessageId(messenger, draft);
    tab = await messenger.compose.beginReply(messageId, "replyToSender", details);
  } else {
    const to = Array.isArray(draft.to) ? draft.to.filter((s) => typeof s === "string" && s.trim()) : [];
    if (!to.length) throw new Error("draft of kind 'new' has no recipients");
    const newDetails = { ...details, to, subject: typeof draft.subject === "string" ? draft.subject : "" };
    if (identity) newDetails.identityId = identity.id;
    tab = await messenger.compose.beginNew(newDetails);
  }
  if (!tab || tab.id === undefined) throw new Error("compose window did not open");

  try {
    // The reply identity is picked by Thunderbird; if its format differs from
    // our guess, rewrite the body in the format the window actually uses.
    if (typeof messenger.compose.getComposeDetails === "function") {
      try {
        const actual = await messenger.compose.getComposeDetails(tab.id);
        if (actual && typeof actual.isPlainText === "boolean" && actual.isPlainText === composeHtml) {
          await messenger.compose.setComposeDetails(tab.id, actual.isPlainText ? { plainTextBody: text } : { body: textToHtml(text) });
        }
      } catch {
        /* best effort */
      }
    }

    let saved;
    try {
      saved = await messenger.compose.saveMessage(tab.id, { mode: "draft" });
    } catch (firstErr) {
      // The editor may not be fully initialised yet; retry once.
      await sleep(saveRetryDelayMs);
      try {
        saved = await messenger.compose.saveMessage(tab.id, { mode: "draft" });
      } catch (err) {
        throw new Error(`saveMessage failed: ${errMsg(err)} (first attempt: ${errMsg(firstErr)})`);
      }
    }
    const id = saved && Array.isArray(saved.messages) && saved.messages[0] ? saved.messages[0].id : null;
    return { tbDraftMessageId: Number.isInteger(id) ? id : null };
  } finally {
    await closeComposeTab(messenger, tab);
  }
}

// ---------------------------------------------------------------------------
// DraftProcessor: save + ack, idempotent across redeliveries
// ---------------------------------------------------------------------------

export class DraftProcessor {
  constructor({
    api,
    messenger,
    storage,
    sleep = defaultSleep,
    log = () => {},
    notify = async () => {},
    onEvent = () => {},
    now = () => Date.now(),
    maxRemembered = 500,
    saveRetryDelayMs = 750,
  }) {
    Object.assign(this, { api, messenger, storage, sleep, log, notify, onEvent, now, maxRemembered, saveRetryDelayMs });
    this.mutex = createMutex();
    this.saved = new Map(); // draft_id -> { tbDraftMessageId, at }
    this.pendingAcks = []; // [{ draftId, body }]
  }

  async load() {
    const got = await this.storage.get([SAVED_DRAFTS_KEY, PENDING_ACKS_KEY]);
    const saved = got?.[SAVED_DRAFTS_KEY];
    this.saved = new Map(Array.isArray(saved) ? saved.filter((e) => Array.isArray(e) && e.length === 2) : []);
    const acks = got?.[PENDING_ACKS_KEY];
    this.pendingAcks = Array.isArray(acks) ? acks.filter((a) => a && a.draftId && a.body) : [];
    return this;
  }

  async _persist() {
    while (this.saved.size > this.maxRemembered) this.saved.delete(this.saved.keys().next().value);
    await this.storage.set({ [SAVED_DRAFTS_KEY]: [...this.saved.entries()], [PENDING_ACKS_KEY]: this.pendingAcks });
  }

  async _ack(draftId, body) {
    try {
      await this.api.ackDraft(draftId, body);
      return true;
    } catch (err) {
      if (isRetryableError(err) || isAuthError(err)) {
        this.log("warn", `ack for draft ${draftId} failed, will retry: ${errMsg(err)}`);
        this.pendingAcks = this.pendingAcks.filter((a) => a.draftId !== draftId);
        this.pendingAcks.push({ draftId, body });
      } else {
        this.log("error", `ack for draft ${draftId} rejected: ${errMsg(err)}`);
      }
      await this._persist();
      return false;
    }
  }

  /** Retry acks that could not be delivered earlier. Returns number still pending. */
  async flushPendingAcks() {
    if (!this.pendingAcks.length) return 0;
    const queue = this.pendingAcks;
    this.pendingAcks = [];
    for (let i = 0; i < queue.length; i++) {
      const a = queue[i];
      try {
        await this.api.ackDraft(a.draftId, a.body);
      } catch (err) {
        if (isRetryableError(err) || isAuthError(err)) {
          this.pendingAcks.push(...queue.slice(i));
          break;
        }
        this.log("error", `dropping ack for draft ${a.draftId}: ${errMsg(err)}`);
      }
    }
    await this._persist();
    return this.pendingAcks.length;
  }

  /** Handle one draft from GET /api/v1/drafts/pending. */
  async process(draft) {
    if (!draft || typeof draft !== "object" || !draft.draft_id) {
      this.log("warn", "ignoring draft without draft_id");
      return { status: "skipped" };
    }
    const draftId = String(draft.draft_id);

    // Already saved but the ack was lost: re-ack instead of creating a second draft.
    const prior = this.saved.get(draftId);
    if (prior) {
      await this._ack(draftId, { status: "saved", tb_draft_message_id: prior.tbDraftMessageId ?? null, error: null });
      return { status: "saved", duplicate: true, tbDraftMessageId: prior.tbDraftMessageId ?? null };
    }

    let result;
    try {
      result = await this.mutex.run(() => saveDraft(draft, { messenger: this.messenger, sleep: this.sleep, saveRetryDelayMs: this.saveRetryDelayMs }));
    } catch (err) {
      const error = errMsg(err).slice(0, 1000);
      this.log("error", `draft ${draftId} failed: ${error}`);
      await this._ack(draftId, { status: "failed", tb_draft_message_id: null, error });
      this.onEvent({ type: "draft_failed", draftId, error });
      return { status: "failed", error };
    }

    this.saved.set(draftId, { tbDraftMessageId: result.tbDraftMessageId, at: this.now() });
    await this._persist();
    await this._ack(draftId, { status: "saved", tb_draft_message_id: result.tbDraftMessageId, error: null });
    this.onEvent({ type: "draft_saved", draftId, subject: draft.subject || "", review: draft.validation === REVIEW_REQUIRED });
    try {
      await this.notify(draft);
    } catch (err) {
      this.log("warn", `notification failed: ${errMsg(err)}`);
    }
    return { status: "saved", tbDraftMessageId: result.tbDraftMessageId };
  }
}

// ---------------------------------------------------------------------------
// DraftPoller: sequential long-poll loop with 5s -> 60s backoff on errors
// ---------------------------------------------------------------------------

export class DraftPoller {
  constructor({
    api,
    processor,
    getConfig,
    sleep = defaultSleep,
    log = () => {},
    onStatus = () => {},
    now = () => Date.now(),
    waitSeconds = 25,
    minBackoffMs = 5000,
    maxBackoffMs = 60000,
    minIntervalMs = 1000,
    idleMs = 5000,
  }) {
    Object.assign(this, { api, processor, getConfig, sleep, log, onStatus, now, waitSeconds, minBackoffMs, maxBackoffMs, minIntervalMs, idleMs });
    this.running = false;
    this.backoffMs = 0;
    this._controller = null;
    this._loop = null;
  }

  nextBackoff() {
    this.backoffMs = this.backoffMs ? Math.min(this.backoffMs * 2, this.maxBackoffMs) : this.minBackoffMs;
    return this.backoffMs;
  }

  /** Start the loop (idempotent). */
  start() {
    if (this.running) return this._loop;
    this.running = true;
    this._loop = (async () => {
      while (this.running) {
        this._controller = new AbortController();
        try {
          await this.runOnce(this._controller.signal);
        } catch (err) {
          // Never let the loop die.
          this.log("error", `poll loop error: ${errMsg(err)}`);
          if (this.running) await this.sleep(this.nextBackoff(), this._controller.signal);
        }
      }
    })();
    return this._loop;
  }

  stop() {
    this.running = false;
    if (this._controller) this._controller.abort();
    return this._loop;
  }

  /** Abort the in-flight long poll / backoff sleep so the next iteration uses fresh config. */
  wake() {
    this.backoffMs = 0;
    if (this._controller) this._controller.abort();
  }

  /** One iteration. Returns 'not_configured' | 'error' | 'aborted' | 'empty' | 'drafts'. */
  async runOnce(signal = new AbortController().signal) {
    const cfg = await this.getConfig();
    if (!cfg.apiToken) {
      this.onStatus({ type: "not_configured" });
      await this.sleep(this.idleMs, signal);
      return "not_configured";
    }

    try {
      await this.processor.flushPendingAcks();
    } catch (err) {
      this.log("warn", `pending ack flush failed: ${errMsg(err)}`);
    }

    const started = this.now();
    let res;
    try {
      res = await this.api.pendingDrafts(this.waitSeconds, { signal });
    } catch (err) {
      if (signal.aborted || err?.aborted) return "aborted";
      const delay = isAuthError(err) ? (this.backoffMs = this.maxBackoffMs) : this.nextBackoff();
      this.onStatus({ type: "error", error: err, retryInMs: delay });
      this.log("warn", `long poll failed: ${errMsg(err)}; retrying in ${delay} ms`);
      await this.sleep(delay, signal);
      return "error";
    }

    const recovered = this.backoffMs !== 0;
    this.backoffMs = 0;
    this.onStatus({ type: "ok", recovered });

    const drafts = Array.isArray(res?.drafts) ? res.drafts : [];
    for (const draft of drafts) {
      if (!this.running && this._loop) break;
      await this.processor.process(draft);
    }
    // Guard against a backend that ignores `wait` and answers instantly.
    if (!drafts.length && this.now() - started < this.minIntervalMs) await this.sleep(this.minIntervalMs, signal);
    return drafts.length ? "drafts" : "empty";
  }
}
