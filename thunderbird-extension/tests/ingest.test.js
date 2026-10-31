import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { createIngestor, iterateMessageList } from "../src/ingest.js";
import { DedupeStore } from "../src/dedupe.js";
import { Outbox } from "../src/outbox.js";
import { normalizeConfig } from "../src/config.js";
import { MemoryStorage } from "./_fakes.js";

const inbox = (accountId) => ({ accountId, path: "/INBOX", name: "Inbox", type: "inbox" });

function hdr(id, accountId = "account1", over = {}) {
  return {
    id,
    headerMessageId: `msg${id}@example.com`,
    author: "Jane <jane@acme.com>",
    subject: `Subject ${id}`,
    recipients: ["me@example.com"],
    ccList: [],
    date: new Date("2026-10-18T09:30:00Z"),
    junk: false,
    read: false,
    folder: inbox(accountId),
    ...over,
  };
}

function setup({ config = {}, pages = {}, accounts = null } = {}) {
  const storage = new MemoryStorage();
  const calls = { getFull: [], continueList: [] };
  const messenger = {
    accounts: {
      async get(id) {
        const map = {
          account1: { id: "account1", name: "me@gmail.com", type: "imap", identities: [{ id: "id1", email: "me@gmail.com" }] },
          account2: { id: "account2", name: "Work", type: "imap", identities: [{ id: "id2", email: "me@corp.example" }] },
          account3: { id: "account3", name: "Home", type: "pop3", identities: [{ id: "id3", email: "me@hotmail.com" }] },
        };
        return map[id];
      },
      async list() {
        return accounts || [];
      },
    },
    identities: {
      async getDefault(id) {
        return (await messenger.accounts.get(id))?.identities[0];
      },
    },
    messages: {
      async getFull(id) {
        calls.getFull.push(id);
        return {
          contentType: "text/plain",
          headers: { "message-id": [`<msg${id}@example.com>`], references: ["<a@x> <b@x>"] },
          body: `body ${id}`,
        };
      },
      async listAttachments() {
        return [];
      },
      async continueList(id) {
        calls.continueList.push(id);
        return pages[id];
      },
      async list(folder) {
        return pages[`list:${folder.accountId}`];
      },
    },
  };
  const cfg = normalizeConfig(config);
  const dedupe = new DedupeStore({ storage, debounceMs: 0 });
  const outbox = new Outbox({ storage, send: async () => ({ status: 202, data: { status: "queued" } }) });
  const ing = createIngestor({ messenger, getConfig: async () => cfg, dedupe, outbox });
  return { ing, outbox, dedupe, calls, messenger };
}

describe("ingest", () => {
  test("follows messageList pagination via continueList", async () => {
    const pages = { p2: { id: "p3", messages: [hdr(3)] }, p3: { id: null, messages: [hdr(4)] } };
    const { ing, outbox, calls } = setup({ pages });
    const counts = await ing.handleNewMail(inbox("account1"), { id: "p2", messages: [hdr(1), hdr(2)] });
    assert.deepEqual(counts, { queued: 4 });
    assert.deepEqual(calls.continueList, ["p2", "p3"]);
    assert.deepEqual(outbox.items.map((i) => i.payload.tb_message_id), [1, 2, 3, 4]);
  });

  test("payload has inferred account type, identity e-mail and parsed headers", async () => {
    const { ing, outbox } = setup();
    await ing.handleNewMail(inbox("account1"), { id: null, messages: [hdr(1)] });
    const p = outbox.items[0].payload;
    assert.equal(p.account_id, "account1");
    assert.equal(p.account_type, "gmail");
    assert.equal(p.account_email, "me@gmail.com");
    assert.equal(p.account_name, "me@gmail.com");
    assert.equal(p.message_id, "<msg1@example.com>");
    assert.deepEqual(p.references, ["<a@x>", "<b@x>"]);
    assert.equal(p.folder, "INBOX");
    assert.equal(outbox.items[0].key, "account1|<msg1@example.com>");
  });

  test("multi-account: type per account, override wins", async () => {
    const { ing, outbox } = setup({ config: { accountTypeOverrides: { account2: "gmail" } } });
    await ing.handleNewMail(inbox("account2"), { messages: [hdr(1, "account2")] });
    await ing.handleNewMail(inbox("account3"), { messages: [hdr(2, "account3")] });
    assert.deepEqual(outbox.items.map((i) => [i.payload.account_id, i.payload.account_type]), [["account2", "gmail"], ["account3", "outlook"]]);
  });

  test("skips junk, special folders and disabled accounts", async () => {
    const { ing, outbox, calls } = setup({ config: { accounts: { account2: false } } });
    assert.deepEqual(await ing.handleNewMail({ accountId: "account1", path: "/Junk", type: "junk" }, { messages: [hdr(1)] }), { skipped_folder: 1 });
    assert.deepEqual(await ing.handleNewMail(inbox("account1"), { messages: [hdr(2, "account1", { junk: true })] }), { skipped_junk: 1 });
    assert.deepEqual(await ing.handleNewMail(inbox("account2"), { messages: [hdr(3, "account2")] }), { skipped_account: 1 });
    assert.equal(
      await ing.ingestMessage(hdr(4, "account1", { folder: { accountId: "account1", path: "/Sent", specialUse: ["sent"] } })),
      "skipped_folder",
    );
    assert.equal(outbox.size, 0);
    assert.deepEqual(calls.getFull, []);
  });

  test("client-side dedupe by accountId|message-id (no second getFull)", async () => {
    const { ing, outbox, calls } = setup();
    await ing.handleNewMail(inbox("account1"), { messages: [hdr(1)] });
    const again = await ing.handleNewMail(inbox("account1"), { messages: [hdr(1)] });
    assert.deepEqual(again, { duplicate: 1 });
    assert.equal(calls.getFull.length, 1);
    // Same Message-ID in another account is a different key
    const other = await ing.handleNewMail(inbox("account2"), { messages: [{ ...hdr(1, "account2") }] });
    assert.deepEqual(other, { queued: 1 });
    assert.equal(outbox.size, 2);
  });

  test("concurrent events for the same message are processed once", async () => {
    const { ing, calls } = setup();
    const r = await Promise.all([ing.ingestMessage(hdr(9)), ing.ingestMessage(hdr(9))]);
    assert.deepEqual(r.sort(), ["duplicate", "queued"]);
    assert.equal(calls.getFull.length, 1);
  });

  test("invalid payload (no sender) is not queued", async () => {
    const { ing, outbox, messenger } = setup();
    messenger.messages.getFull = async () => ({ contentType: "text/plain", headers: {}, body: "" });
    assert.equal(await ing.ingestMessage(hdr(5, "account1", { author: "" })), "invalid");
    assert.equal(outbox.size, 0);
  });

  test("getFull failure is reported as error and can be retried later", async () => {
    const { ing, messenger, dedupe } = setup();
    messenger.messages.getFull = async () => {
      throw new Error("message deleted");
    };
    assert.equal(await ing.ingestMessage(hdr(6)), "error");
    assert.equal(dedupe.size, 0);
  });

  test("processExistingUnread scans only unread INBOX mail of enabled accounts", async () => {
    const accounts = [
      { id: "account1", folders: [{ accountId: "account1", path: "/INBOX", type: "inbox" }, { accountId: "account1", path: "/Sent", type: "sent" }] },
      { id: "account2", folders: [{ accountId: "account2", path: "/INBOX", type: "inbox" }] },
    ];
    const pages = {
      "list:account1": { id: "more", messages: [hdr(1), hdr(2, "account1", { read: true })] },
      more: { id: null, messages: [hdr(3)] },
      "list:account2": { id: null, messages: [hdr(4, "account2")] },
    };
    const { ing, outbox } = setup({ pages, accounts, config: { accounts: { account2: false } } });
    const totals = await ing.processExistingUnread();
    assert.equal(totals.accounts, 1);
    assert.equal(totals.scanned, 2);
    assert.equal(totals.queued, 2);
    assert.deepEqual(outbox.items.map((i) => i.payload.tb_message_id), [1, 3]);
  });

  test("iterateMessageList stops on repeated page ids", async () => {
    const messenger = { messages: { continueList: async () => ({ id: "same", messages: [{ id: 2 }] }) } };
    const out = [];
    for await (const m of iterateMessageList(messenger, { id: "same", messages: [{ id: 1 }] })) out.push(m.id);
    assert.deepEqual(out, [1, 2]);
  });
});

import { isExcludedSender } from "../src/ingest.js";

test("excluded senders: addresses and domains, subdomains included", () => {
  const list = ["hdfcbank.com", "@icicibank.com", "alerts@paytm.com"];
  assert.equal(isExcludedSender(list, "HDFC Bank <InstaAlerts@hdfcbank.com>"), true);
  assert.equal(isExcludedSender(list, "noreply@mail.hdfcbank.com"), true);
  assert.equal(isExcludedSender(list, "x@icicibank.com"), true);
  assert.equal(isExcludedSender(list, "alerts@paytm.com"), true);
  assert.equal(isExcludedSender(list, "support@paytm.com"), false);
  assert.equal(isExcludedSender(list, "me@nothdfcbank.com"), false);
  assert.equal(isExcludedSender([], "a@hdfcbank.com"), false);
});
