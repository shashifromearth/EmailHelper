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

const normEmail = (s) => (typeof s === "string" ? s.trim().toLowerCase() : "");

/** All identities of one account: identities.list(accountId) -> accounts.get(accountId).identities. */
async function listAccountIdentities(messenger, accountId) {
  let list = null;
  if (typeof messenger.identities?.list === "function") {
    try {
      list = await messenger.identities.list(accountId);
    } catch {
      list = null;
    }
  }
  if (!Array.isArray(list)) {
    try {
      list = (await messenger.accounts?.get?.(accountId, false))?.identities;
    } catch {
      list = null;
    }
  }
  // Never accept an identity that says it belongs to another account.
  return Array.isArray(list) ? list.filter((i) => i && i.id && (i.accountId === undefined || i.accountId === accountId)) : [];
}

/**
 * Identity that must send the draft: the identity of `accountId` whose e-mail
 * equals `fromAddress` (case-insensitive), else the account's default identity.
 * Never returns an identity of a different account.
 */
export async function resolveSenderIdentity(messenger, accountId, fromAddress) {
  if (!accountId) return null;
  const wanted = normEmail(fromAddress);
  if (wanted) {
    const match = (await listAccountIdentities(messenger, accountId)).find((i) => normEmail(i.email) === wanted);
    if (match) return match;
  }
  return resolveIdentity(messenger, accountId);
}

/**
 * Thunderbird message id to reply to. Verifies `tb_message_id` still refers to
 * the same Message-ID in the draft's account (ids can change when mail is moved
 * or the index is rebuilt) and otherwise looks the message up by its Message-ID
 * header. Only copies in `draft.account_id` are ever used: a BCC copy of the
 * same mail in another mailbox would make Thunderbird reply from that mailbox.
 */
export async function resolveReplyMessageId(messenger, draft) {
  const wanted = stripAngleBrackets(draft.message_id || "");
  const tbId = Number.isInteger(draft.tb_message_id) ? draft.tb_message_id : null;

  if (tbId !== null) {
    if (typeof messenger.messages?.get !== "function") return tbId;
    try {
      const h = await messenger.messages.get(tbId);
      const sameMessage = h && (!wanted || stripAngleBrackets(h.headerMessageId || "") === wanted);
      const accountId = h?.folder?.accountId;
      const sameAccount = accountId === undefined || accountId === null || accountId === draft.account_id;
      if (sameMessage && sameAccount) return h.id ?? tbId;
    } catch {
      /* fall through to lookup */
    }
  }

  let otherAccounts = [];
  if (wanted && typeof messenger.messages?.query === "function") {
    try {
      const page = await messenger.messages.query({ headerMessageId: wanted });
      const found = (page?.messages || []).filter((m) => stripAngleBrackets(m.headerMessageId || "") === wanted);
      const sameAccount = found.filter((m) => m.folder?.accountId === draft.account_id);
      const pick = sameAccount.find((m) => !shouldSkipFolder(m.folder)) || sameAccount[0];
      if (pick) return pick.id;
      otherAccounts = [...new Set(found.map((m) => m.folder?.accountId).filter(Boolean))];
    } catch {
      /* not supported or failed */
    }
  }
  const elsewhere = otherAccounts.length ? `; copies only in other accounts: ${otherAccounts.join(", ")}` : "";
  throw new Error(
    `original message not found in account ${draft.account_id} (tb_message_id=${draft.tb_message_id}, message_id=${draft.message_id || "null"}${elsewhere})`,
  );
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
export async function saveDraft(draft, { messenger, sleep = defaultSleep, saveRetryDelayMs = 750, log = () => {} } = {}) {
  if (draft.kind !== "reply" && draft.kind !== "new") throw new Error(`unsupported draft kind: ${draft.kind}`);

  const text = buildDraftBody(draft);
  const fromAddress = typeof draft.from_address === "string" ? draft.from_address : null;
  const identity = await resolveSenderIdentity(messenger, draft.account_id, fromAddress);
  if (fromAddress && normEmail(fromAddress) && normEmail(identity?.email) !== normEmail(fromAddress)) {
    log(
      "warn",
      `draft ${draft.draft_id}: account ${draft.account_id} has no identity for ${fromAddress}; using ${identity?.email || "the account default"}`,
    );
  }
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
    const canRead = typeof messenger.compose.getComposeDetails === "function";

    // beginReply lets Thunderbird pick the sender; force the identity of the
    // mailbox that received the mail. Failing here fails the draft (no wrong sender).
    if (draft.kind === "reply" && identity) {
      let current;
      if (canRead) {
        try {
          current = (await messenger.compose.getComposeDetails(tab.id))?.identityId;
        } catch {
          current = undefined;
        }
      }
      if (current !== identity.id) {
        try {
          await messenger.compose.setComposeDetails(tab.id, { identityId: identity.id });
        } catch (err) {
          throw new Error(`could not set sender identity ${identity.email || identity.id}: ${errMsg(err)}`);
        }
      }
    }

    // If the window's format differs from our guess, rewrite the body in the
    // format the window actually uses.
    if (canRead) {
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
      result = await this.mutex.run(() =>
        saveDraft(draft, { messenger: this.messenger, sleep: this.sleep, saveRetryDelayMs: this.saveRetryDelayMs, log: this.log }),
      );
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
