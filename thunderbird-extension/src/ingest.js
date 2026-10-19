// New-mail ingestion: messages.onNewMailReceived -> getFull -> payload ->
// outbox. `messenger` is injected so this runs against a fake in tests.

import {
  buildEmailPayload,
  getFolderAccountId,
  inferAccountType,
  isJunk,
  normalizeMessageId,
  shouldSkipFolder,
  validateEmailPayload,
  ACCOUNT_TYPES,
} from "./extract.js";
import { isAccountEnabled } from "./config.js";
import { dedupeKey } from "./dedupe.js";

const errMsg = (e) => (e && e.message ? e.message : String(e));

/** Iterate all MessageHeaders of a MessageList, following `continueList` pagination. */
export async function* iterateMessageList(messenger, list) {
  let page = list;
  const seenPages = new Set();
  while (page) {
    for (const m of Array.isArray(page.messages) ? page.messages : []) yield m;
    if (!page.id || seenPages.has(page.id)) break;
    seenPages.add(page.id);
    page = await messenger.messages.continueList(page.id);
  }
}

export function createIngestor({ messenger, getConfig, dedupe, outbox, log = () => {}, onEvent = () => {}, accountCacheMs = 5 * 60 * 1000, now = () => Date.now() }) {
  const accountCache = new Map(); // accountId -> { at, info }
  const inFlight = new Set();

  async function getAccountInfo(accountId) {
    const cached = accountCache.get(accountId);
    if (cached && now() - cached.at < accountCacheMs) return cached.info;
    let account = null;
    try {
      account = await messenger.accounts.get(accountId, false);
    } catch (err) {
      log("warn", `accounts.get(${accountId}) failed: ${errMsg(err)}`);
    }
    let identity = null;
    try {
      identity = (await messenger.identities?.getDefault?.(accountId)) || null;
    } catch {
      /* older TB */
    }
    const identities = Array.isArray(account?.identities) ? account.identities : [];
    identity = identity || identities[0] || null;
    const info = {
      id: accountId,
      name: account?.name ?? null,
      email: identity?.email ?? null,
      protocol: account?.type ?? null,
      inferredType: inferAccountType({
        hostname: account?.incomingServer?.hostname ?? account?.hostname ?? null,
        type: account?.type,
        emails: [identity?.email, ...identities.map((i) => i.email)].filter(Boolean),
        name: account?.name,
      }),
    };
    accountCache.set(accountId, { at: now(), info });
    return info;
  }

  function invalidateAccounts() {
    accountCache.clear();
  }

  /**
   * Ingest one MessageHeader. Returns a status string:
   * queued | duplicate | skipped_folder | skipped_junk | skipped_account | invalid | error
   */
  async function ingestMessage(header, folderHint = null) {
    const folder = header?.folder || folderHint;
    if (!header || !Number.isInteger(header.id)) return "invalid";
    if (shouldSkipFolder(folder)) return "skipped_folder";
    if (isJunk(header)) return "skipped_junk";
    const accountId = getFolderAccountId(folder);
    const cfg = await getConfig();
    if (!isAccountEnabled(cfg, accountId)) return "skipped_account";

    const quickId = normalizeMessageId(header.headerMessageId) || `<tb-${header.id}@thunderbird.invalid>`;
    const key = dedupeKey(accountId, quickId);
    if (dedupe.has(key) || outbox.has(key) || inFlight.has(key)) return "duplicate";
    inFlight.add(key);
    try {
      const full = await messenger.messages.getFull(header.id);
      let attachments = null;
      if (typeof messenger.messages.listAttachments === "function") {
        try {
          attachments = await messenger.messages.listAttachments(header.id);
        } catch {
          attachments = null; // fall back to walking the MIME tree
        }
      }
      const info = await getAccountInfo(accountId);
      const override = cfg.accountTypeOverrides?.[accountId];
      const account = {
        id: accountId,
        name: info.name,
        email: info.email,
        type: ACCOUNT_TYPES.includes(override) ? override : info.inferredType,
      };
      const payload = buildEmailPayload({ header, full, attachments, account, folder });
      const problems = validateEmailPayload(payload);
      if (problems.length) {
        log("warn", `skipping message ${header.id}: ${problems.join("; ")}`);
        onEvent({ type: "invalid", tbMessageId: header.id, problems });
        await dedupe.add(key);
        return "invalid";
      }
      await outbox.enqueue(key, payload);
      await dedupe.add(key);
      onEvent({ type: "queued", key });
      return "queued";
    } catch (err) {
      log("error", `failed to read message ${header.id}: ${errMsg(err)}`);
      onEvent({ type: "error", error: err });
      return "error";
    } finally {
      inFlight.delete(key);
    }
  }

  /** onNewMailReceived listener body. Returns counts per status. */
  async function handleNewMail(folder, messageList) {
    const counts = {};
    if (shouldSkipFolder(folder)) return { skipped_folder: messageList?.messages?.length ?? 0 };
    for await (const header of iterateMessageList(messenger, messageList)) {
      const status = await ingestMessage(header, folder);
      counts[status] = (counts[status] || 0) + 1;
    }
    return counts;
  }

  function findInboxes(folders, out = []) {
    for (const f of Array.isArray(folders) ? folders : []) {
      const isInbox = f.type === "inbox" || (Array.isArray(f.specialUse) && f.specialUse.includes("inbox"));
      if (isInbox) out.push(f);
      if (Array.isArray(f.subFolders)) findInboxes(f.subFolders, out);
    }
    return out;
  }

  /** One-off: queue unread messages in the INBOX of every enabled account. */
  async function processExistingUnread({ limitPerAccount = 200 } = {}) {
    const cfg = await getConfig();
    const totals = { accounts: 0, scanned: 0 };
    const accounts = (await messenger.accounts.list()) || [];
    for (const acc of accounts) {
      if (!isAccountEnabled(cfg, acc.id)) continue;
      let folders = acc.folders || acc.rootFolder?.subFolders;
      if (!folders) {
        try {
          const full = await messenger.accounts.get(acc.id, true);
          folders = full?.folders || full?.rootFolder?.subFolders || [];
        } catch {
          folders = [];
        }
      }
      const inboxes = findInboxes(folders);
      if (!inboxes.length) continue;
      totals.accounts++;
      let taken = 0;
      for (const inbox of inboxes) {
        // MV2 accepts a MailFolder object on both TB115 and TB128.
        const list = await messenger.messages.list(inbox);
        for await (const header of iterateMessageList(messenger, list)) {
          if (header.read) continue;
          totals.scanned++;
          const status = await ingestMessage({ ...header, folder: header.folder || inbox }, inbox);
          totals[status] = (totals[status] || 0) + 1;
          if (++taken >= limitPerAccount) break;
        }
        if (taken >= limitPerAccount) break;
      }
    }
    return totals;
  }

  return { ingestMessage, handleNewMail, processExistingUnread, getAccountInfo, invalidateAccounts };
}
