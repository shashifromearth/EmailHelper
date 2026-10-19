import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, dirname, relative } from "node:path";
import { fileURLToPath } from "node:url";
import {
  DraftProcessor,
  DraftPoller,
  buildDraftBody,
  reviewBanner,
  textToHtml,
  createMutex,
  resolveReplyMessageId,
  resolveSenderIdentity,
  SAVED_DRAFTS_KEY,
  PENDING_ACKS_KEY,
} from "../src/drafts.js";
import { ApiError } from "../src/api.js";
import { MemoryStorage, fakeMessenger, recordingSleep, noSleep } from "./_fakes.js";

const replyDraft = (over = {}) => ({
  draft_id: "d7c2",
  kind: "reply",
  account_id: "account3",
  tb_message_id: 1234,
  message_id: "<CAF=abc@mail.gmail.com>",
  to: ["jane@acme.com"],
  subject: "Re: Question about pricing",
  body: "Hi Jane,\n\nThanks <for> asking & more.",
  validation: "APPROVED",
  notes: ["Grounded in approved_responses/pricing.md"],
  ...over,
});

const newDraft = (over = {}) => ({
  draft_id: "n1",
  kind: "new",
  account_id: "account3",
  tb_message_id: null,
  message_id: null,
  to: ["recruiter@acme.com"],
  subject: "Staff AI Engineer role",
  body: "Hello,\nI am interested.",
  validation: "APPROVED",
  notes: [],
  ...over,
});

const plainIdentity = { id: "id3", email: "me@example.com", composeHtml: false };
const htmlIdentity = { id: "id3", email: "me@example.com", composeHtml: true };
const original = { 1234: { id: 1234, headerMessageId: "CAF=abc@mail.gmail.com", folder: { accountId: "account3", type: "inbox", path: "/INBOX" } } };

function fakeApi({ ackFails = 0, ackError = null } = {}) {
  const acks = [];
  let failures = ackFails;
  return {
    acks,
    async ackDraft(id, body) {
      if (ackError) throw ackError;
      if (failures > 0) {
        failures--;
        throw new ApiError("down", { network: true });
      }
      acks.push({ id, body });
      return { status: 204, data: null };
    },
  };
}

function makeProcessor({ messenger, api = fakeApi(), storage = new MemoryStorage(), notified = [] } = {}) {
  const events = [];
  const logs = [];
  const p = new DraftProcessor({
    api,
    messenger,
    storage,
    sleep: noSleep,
    log: (level, msg) => logs.push({ level, msg }),
    notify: async (d) => notified.push(d.subject),
    onEvent: (e) => events.push(e),
  });
  return { p, api, storage, events, notified, logs };
}

describe("draft body", () => {
  test("APPROVED body is unchanged (CRLF normalised)", () => {
    assert.equal(buildDraftBody(replyDraft({ body: "a\r\nb" })), "a\nb");
  });

  test("REVIEW_REQUIRED banner on top, notes joined with '; '", () => {
    const body = buildDraftBody(replyDraft({ validation: "REVIEW_REQUIRED", notes: ["Mentions a price", "No source for SLA"], body: "Hi" }));
    assert.equal(body, "[EmailHelper: REVIEW REQUIRED – Mentions a price; No source for SLA]\n\nHi");
  });

  test("banner with no notes still follows the format", () => {
    assert.equal(reviewBanner([]), "[EmailHelper: REVIEW REQUIRED – no reason given]");
  });

  test("textToHtml escapes and keeps line breaks", () => {
    const html = textToHtml("a < b & c\nnext");
    assert.ok(html.includes("a &lt; b &amp; c<br>\nnext"));
    assert.ok(!html.includes("a < b"));
  });
});

describe("DraftProcessor", () => {
  test("kind=reply uses beginReply(tb_message_id, 'replyToSender') with plainTextBody for plain-text identities, saves, closes, acks", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original });
    const { p, api, events, notified } = makeProcessor({ messenger });
    const r = await p.process(replyDraft());
    assert.equal(r.status, "saved");
    assert.equal(r.tbDraftMessageId, 5678);

    const [reply] = messenger.find("compose.beginReply");
    assert.equal(reply.args[0], 1234);
    assert.equal(reply.args[1], "replyToSender");
    assert.deepEqual(reply.args[2], { isPlainText: true, plainTextBody: "Hi Jane,\n\nThanks <for> asking & more." });

    const [save] = messenger.find("compose.saveMessage");
    assert.deepEqual(save.args[1], { mode: "draft" });
    assert.equal(messenger.find("windows.remove").length, 1);
    assert.equal(messenger.find("compose.beginNew").length, 0);

    const names = messenger.names();
    assert.ok(names.indexOf("compose.saveMessage") < names.indexOf("windows.remove"), "window closed after save");

    assert.deepEqual(api.acks, [{ id: "d7c2", body: { status: "saved", tb_draft_message_id: 5678, error: null } }]);
    assert.equal(events[0].type, "draft_saved");
    assert.deepEqual(notified, ["Re: Question about pricing"]);
  });

  test("HTML identity gets an escaped HTML body", async () => {
    const messenger = fakeMessenger({ identities: { account3: htmlIdentity }, messages: original });
    const { p } = makeProcessor({ messenger });
    await p.process(replyDraft());
    const details = messenger.find("compose.beginReply")[0].args[2];
    assert.equal(details.isPlainText, false);
    assert.equal(details.plainTextBody, undefined);
    assert.ok(details.body.includes("Hi Jane,<br>\n<br>\nThanks &lt;for&gt; asking &amp; more."));
  });

  test("compose window format mismatch is corrected via setComposeDetails", async () => {
    const messenger = fakeMessenger({ identities: { account3: htmlIdentity }, messages: original, composeIsPlainText: true });
    const { p } = makeProcessor({ messenger });
    await p.process(replyDraft());
    const [set] = messenger.find("compose.setComposeDetails");
    assert.deepEqual(set.args[1], { plainTextBody: "Hi Jane,\n\nThanks <for> asking & more." });
  });

  test("kind=new uses beginNew with to/subject/identityId of the account's default identity", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity } });
    const { p, api } = makeProcessor({ messenger });
    const r = await p.process(newDraft());
    assert.equal(r.status, "saved");
    assert.equal(messenger.find("compose.beginReply").length, 0);
    const [n] = messenger.find("compose.beginNew");
    assert.deepEqual(n.args[0], {
      isPlainText: true,
      plainTextBody: "Hello,\nI am interested.",
      to: ["recruiter@acme.com"],
      subject: "Staff AI Engineer role",
      identityId: "id3",
    });
    assert.deepEqual(messenger.find("identities.getDefault")[0].args, ["account3"]);
    assert.equal(api.acks[0].body.status, "saved");
  });

  test("identity falls back to accounts.get().identities[0] when identities.getDefault is unavailable", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity } });
    delete messenger.identities;
    const { p } = makeProcessor({ messenger });
    await p.process(newDraft());
    assert.equal(messenger.find("compose.beginNew")[0].args[0].identityId, "id3");
  });

  test("REVIEW_REQUIRED banner is prepended in the saved draft", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original });
    const { p, events } = makeProcessor({ messenger });
    await p.process(replyDraft({ validation: "REVIEW_REQUIRED", notes: ["Unverified claim", "Check price"] }));
    const details = messenger.find("compose.beginReply")[0].args[2];
    assert.ok(details.plainTextBody.startsWith("[EmailHelper: REVIEW REQUIRED – Unverified claim; Check price]\n\nHi Jane,"));
    assert.equal(events[0].review, true);
  });

  test("save failure acks status=failed with the error and still closes the window", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original, saveError: "disk full" });
    const { p, api, events, notified } = makeProcessor({ messenger });
    const r = await p.process(replyDraft());
    assert.equal(r.status, "failed");
    assert.equal(messenger.find("compose.saveMessage").length, 2, "save retried once");
    assert.equal(messenger.find("windows.remove").length, 1);
    assert.equal(api.acks.length, 1);
    assert.equal(api.acks[0].body.status, "failed");
    assert.equal(api.acks[0].body.tb_draft_message_id, null);
    assert.match(api.acks[0].body.error, /disk full/);
    assert.equal(events[0].type, "draft_failed");
    assert.deepEqual(notified, []);
  });

  test("missing original message acks failed (never replies to the wrong mail)", async () => {
    const messenger = fakeMessenger({
      identities: { account3: plainIdentity },
      messages: { 1234: { id: 1234, headerMessageId: "someone-else@x" } },
    });
    const { p, api } = makeProcessor({ messenger });
    const r = await p.process(replyDraft());
    assert.equal(r.status, "failed");
    assert.equal(messenger.find("compose.beginReply").length, 0);
    assert.match(api.acks[0].body.error, /original message not found/);
  });

  test("stale tb_message_id is resolved by Message-ID lookup", async () => {
    const messenger = fakeMessenger({
      messages: {
        1234: { id: 1234, headerMessageId: "other@x" },
        9999: { id: 9999, headerMessageId: "CAF=abc@mail.gmail.com", folder: { accountId: "account3", type: "inbox", path: "/INBOX" } },
        9998: { id: 9998, headerMessageId: "CAF=abc@mail.gmail.com", folder: { accountId: "account3", type: "sent", path: "/Sent" } },
      },
    });
    assert.equal(await resolveReplyMessageId(messenger, replyDraft()), 9999);
  });

  test("unsupported kind and missing recipients fail cleanly", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity } });
    const { p, api } = makeProcessor({ messenger });
    await p.process(newDraft({ draft_id: "a", kind: "forward" }));
    await p.process(newDraft({ draft_id: "b", to: [] }));
    assert.deepEqual(api.acks.map((a) => [a.id, a.body.status]), [["a", "failed"], ["b", "failed"]]);
    assert.match(api.acks[0].body.error, /unsupported draft kind/);
    assert.equal(messenger.find("compose.beginNew").length, 0);
  });

  test("draft without draft_id is skipped without ack", async () => {
    const messenger = fakeMessenger();
    const { p, api } = makeProcessor({ messenger });
    assert.equal((await p.process({ kind: "new" })).status, "skipped");
    assert.equal(api.acks.length, 0);
  });

  test("lost ack is queued, persisted and retried; redelivered draft is not saved twice", async () => {
    const storage = new MemoryStorage();
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original });
    const api = fakeApi({ ackFails: 1 });
    const { p } = makeProcessor({ messenger, api, storage });
    await p.process(replyDraft());
    assert.equal(api.acks.length, 0);
    assert.equal(storage.data[PENDING_ACKS_KEY].length, 1);
    assert.deepEqual(storage.data[SAVED_DRAFTS_KEY][0][0], "d7c2");

    // Restart: new processor loads state from storage.
    const p2 = new DraftProcessor({ api, messenger, storage, sleep: noSleep });
    await p2.load();
    assert.equal(await p2.flushPendingAcks(), 0);
    assert.deepEqual(api.acks, [{ id: "d7c2", body: { status: "saved", tb_draft_message_id: 5678, error: null } }]);

    // Backend hands the same draft out again (e.g. ack arrived after 10 minutes).
    const r = await p2.process(replyDraft());
    assert.equal(r.duplicate, true);
    assert.equal(messenger.find("compose.beginReply").length, 1, "compose opened only once in total");
    assert.equal(api.acks.length, 2);
  });

  test("ack rejected with 404 is not retried", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original });
    const api = fakeApi({ ackError: new ApiError("not found", { status: 404 }) });
    const { p } = makeProcessor({ messenger, api });
    await p.process(replyDraft());
    assert.equal(p.pendingAcks.length, 0);
  });

  test("drafts are saved one at a time (no overlapping compose windows)", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original });
    let open = 0;
    let maxOpen = 0;
    const origBeginReply = messenger.compose.beginReply;
    messenger.compose.beginReply = async (...a) => {
      open++;
      maxOpen = Math.max(maxOpen, open);
      await new Promise((r) => setTimeout(r, 5));
      return origBeginReply(...a);
    };
    const origRemove = messenger.windows.remove;
    messenger.windows.remove = async (id) => {
      open--;
      return origRemove(id);
    };
    const { p } = makeProcessor({ messenger });
    await Promise.all([p.process(replyDraft({ draft_id: "1" })), p.process(replyDraft({ draft_id: "2" })), p.process(replyDraft({ draft_id: "3" }))]);
    assert.equal(maxOpen, 1);
    assert.equal(messenger.find("compose.saveMessage").length, 3);
  });

  test("createMutex keeps running after a failed job", async () => {
    const m = createMutex();
    await assert.rejects(m.run(async () => {
      throw new Error("x");
    }));
    assert.equal(await m.run(async () => 42), 42);
  });
});

describe("multi-mailbox: replies come from the mailbox that received the mail", () => {
  const MID = "CAF=abc@mail.gmail.com";
  const inbox = (id, accountId, extra = {}) => ({ id, headerMessageId: MID, folder: { accountId, type: "inbox", path: "/INBOX" }, ...extra });
  const mom = { id: "id7", email: "mom@family.org", composeHtml: true };
  const aliasPlain = { id: "id3b", email: "Alias@Family.org", composeHtml: false };

  test("BCC copy only in another account: draft fails, no reply from the other mailbox", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity, account7: mom },
      messages: { 1234: inbox(1234, "account7"), 4321: inbox(4321, "account7") },
    });
    const { p, api, events } = makeProcessor({ messenger });
    const r = await p.process(replyDraft());
    assert.equal(r.status, "failed");
    assert.equal(messenger.find("compose.beginReply").length, 0);
    assert.equal(messenger.find("compose.beginNew").length, 0);
    assert.equal(api.acks[0].body.status, "failed");
    assert.match(api.acks[0].body.error, /original message not found in account account3/);
    assert.match(api.acks[0].body.error, /account7/);
    assert.equal(events[0].type, "draft_failed");
  });

  test("same-account copy found by Message-ID query is used, other-account copies ignored", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity, account7: mom },
      messages: { 500: inbox(500, "account7"), 600: inbox(600, "account3"), 700: inbox(700, "account9") },
    });
    const { p } = makeProcessor({ messenger });
    const r = await p.process(replyDraft({ tb_message_id: null }));
    assert.equal(r.status, "saved");
    assert.equal(messenger.find("compose.beginReply")[0].args[0], 600);
  });

  test("same-account copy in a skipped folder is used only when no usable same-account copy exists", async () => {
    const sent = { id: 610, headerMessageId: MID, folder: { accountId: "account3", type: "sent", path: "/Sent" } };
    const messenger = fakeMessenger({ messages: { 500: inbox(500, "account7"), 610: sent } });
    assert.equal(await resolveReplyMessageId(messenger, replyDraft({ tb_message_id: null })), 610);
  });

  test("tb_message_id pointing at another account's copy is not used; falls back to the same-account copy", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity, account7: mom },
      messages: { 1234: inbox(1234, "account7"), 600: inbox(600, "account3") },
    });
    assert.equal(await resolveReplyMessageId(messenger, replyDraft()), 600);
    assert.deepEqual(messenger.find("messages.get")[0].args, [1234]);
    assert.equal(messenger.find("messages.query").length, 1);

    const { p } = makeProcessor({ messenger });
    await p.process(replyDraft());
    assert.equal(messenger.find("compose.beginReply")[0].args[0], 600);
  });

  test("reply: from_address matching a non-default identity is set via setComposeDetails and decides the format", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity },
      accountIdentities: { account3: [htmlIdentity, aliasPlain] },
      messages: original,
    });
    const { p, logs } = makeProcessor({ messenger });
    const r = await p.process(replyDraft({ from_address: "alias@family.ORG" }));
    assert.equal(r.status, "saved");
    const [reply] = messenger.find("compose.beginReply");
    assert.equal(reply.args[2].isPlainText, true, "format follows the alias identity (plain text)");
    const sets = messenger.find("compose.setComposeDetails").map((c) => c.args[1]);
    assert.deepEqual(sets[0], { identityId: "id3b" });
    const names = messenger.names();
    assert.ok(names.indexOf("compose.setComposeDetails") < names.indexOf("compose.saveMessage"), "identity set before save");
    assert.deepEqual(logs.filter((l) => l.level === "warn"), []);
  });

  test("new: from_address matching a non-default identity is passed to beginNew", async () => {
    const messenger = fakeMessenger({ identities: { account3: htmlIdentity }, accountIdentities: { account3: [htmlIdentity, aliasPlain] } });
    const { p } = makeProcessor({ messenger });
    await p.process(newDraft({ from_address: "alias@family.org" }));
    const [n] = messenger.find("compose.beginNew");
    assert.equal(n.args[0].identityId, "id3b");
    assert.equal(n.args[0].isPlainText, true);
    assert.equal(messenger.find("compose.setComposeDetails").length, 0);
  });

  test("from_address absent or null: account default identity", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity },
      accountIdentities: { account3: [aliasPlain, htmlIdentity] },
      messages: original,
    });
    const { p, logs } = makeProcessor({ messenger });
    await p.process(newDraft({ draft_id: "n-absent" }));
    await p.process(newDraft({ draft_id: "n-null", from_address: null }));
    assert.deepEqual(messenger.find("compose.beginNew").map((c) => c.args[0].identityId), ["id3", "id3"]);

    await p.process(replyDraft());
    // Thunderbird already chose the default identity: nothing to change.
    assert.equal(messenger.find("compose.setComposeDetails").filter((c) => "identityId" in c.args[1]).length, 0);
    assert.deepEqual(logs, []);
  });

  test("reply: identity Thunderbird picked is corrected to the account default when it differs", async () => {
    const messenger = fakeMessenger({ identities: { account3: htmlIdentity }, messages: original });
    const orig = messenger.compose.getComposeDetails;
    let first = true;
    messenger.compose.getComposeDetails = async (tabId) => {
      const d = await orig(tabId);
      if (first) {
        first = false;
        return { ...d, identityId: "id7" };
      }
      return d;
    };
    const { p } = makeProcessor({ messenger });
    await p.process(replyDraft());
    assert.deepEqual(messenger.find("compose.setComposeDetails")[0].args[1], { identityId: "id3" });
  });

  test("from_address of another account is never used: account default instead, with a warning", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity, account7: mom },
      messages: original,
    });
    const { p, logs } = makeProcessor({ messenger });
    await p.process(newDraft({ from_address: "mom@family.org" }));
    assert.equal(messenger.find("compose.beginNew")[0].args[0].identityId, "id3");

    await p.process(replyDraft({ from_address: "mom@family.org" }));
    const identitySets = messenger.find("compose.setComposeDetails").filter((c) => "identityId" in c.args[1]);
    assert.ok(identitySets.every((c) => c.args[1].identityId === "id3"));
    assert.ok(!messenger.find("identities.list").some((c) => c.args[0] === "account7"), "other account's identities never listed");
    const warns = logs.filter((l) => l.level === "warn");
    assert.equal(warns.length, 2);
    assert.match(warns[0].msg, /no identity for mom@family\.org/);
  });

  test("resolveSenderIdentity ignores an identity that reports a different accountId", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity },
      accountIdentities: { account3: [htmlIdentity, { id: "id7", email: "mom@family.org", accountId: "account7" }] },
    });
    assert.equal((await resolveSenderIdentity(messenger, "account3", "mom@family.org")).id, "id3");
    assert.equal((await resolveSenderIdentity(messenger, "account3", "ME@example.com")).id, "id3");
  });

  test("resolveSenderIdentity falls back to accounts.get().identities when identities.list is unavailable", async () => {
    const messenger = fakeMessenger({ identities: { account3: htmlIdentity }, accountIdentities: { account3: [htmlIdentity, aliasPlain] } });
    delete messenger.identities.list;
    assert.equal((await resolveSenderIdentity(messenger, "account3", "alias@family.org")).id, "id3b");
  });

  test("failure to set the reply identity fails the draft and closes the window", async () => {
    const messenger = fakeMessenger({
      identities: { account3: htmlIdentity },
      accountIdentities: { account3: [htmlIdentity, aliasPlain] },
      messages: original,
    });
    messenger.compose.setComposeDetails = async () => {
      throw new Error("boom");
    };
    const { p, api } = makeProcessor({ messenger });
    const r = await p.process(replyDraft({ from_address: "alias@family.org" }));
    assert.equal(r.status, "failed");
    assert.match(api.acks[0].body.error, /could not set sender identity/);
    assert.equal(messenger.find("compose.saveMessage").length, 0);
    assert.equal(messenger.find("windows.remove").length, 1);
  });
});

describe("DraftPoller", () => {
  const cfg = { backendUrl: "http://127.0.0.1:7800", apiToken: "t" };

  test("processes drafts returned by the long poll in order", async () => {
    const processed = [];
    const api = { pendingDrafts: async (wait) => (assert.equal(wait, 25), { drafts: [{ draft_id: "a" }, { draft_id: "b" }] }) };
    const poller = new DraftPoller({
      api,
      processor: { flushPendingAcks: async () => 0, process: async (d) => processed.push(d.draft_id) },
      getConfig: async () => cfg,
      sleep: noSleep,
    });
    assert.equal(await poller.runOnce(), "drafts");
    assert.deepEqual(processed, ["a", "b"]);
  });

  test("network errors back off 5s -> 10s -> 20s -> 40s -> 60s and reset on success", async () => {
    const sleep = recordingSleep();
    let fail = 6;
    const statuses = [];
    const api = {
      pendingDrafts: async () => {
        if (fail-- > 0) throw new ApiError("down", { network: true });
        return { drafts: [] };
      },
    };
    let t = 0;
    const poller = new DraftPoller({
      api,
      processor: { flushPendingAcks: async () => 0, process: async () => {} },
      getConfig: async () => cfg,
      sleep,
      now: () => (t += 30000), // each poll "took" 30 s, so no min-interval sleep
      onStatus: (s) => statuses.push(s.type + (s.recovered ? "+recovered" : "")),
    });
    for (let i = 0; i < 7; i++) await poller.runOnce();
    assert.deepEqual(sleep.delays, [5000, 10000, 20000, 40000, 60000, 60000]);
    assert.equal(poller.backoffMs, 0);
    assert.equal(statuses.at(-1), "ok+recovered");
  });

  test("401 backs off for the maximum interval", async () => {
    const sleep = recordingSleep();
    const poller = new DraftPoller({
      api: { pendingDrafts: async () => Promise.reject(new ApiError("no", { status: 401 })) },
      processor: { flushPendingAcks: async () => 0, process: async () => {} },
      getConfig: async () => cfg,
      sleep,
    });
    assert.equal(await poller.runOnce(), "error");
    assert.deepEqual(sleep.delays, [60000]);
  });

  test("no token: idles without calling the backend", async () => {
    let called = false;
    const poller = new DraftPoller({
      api: { pendingDrafts: async () => ((called = true), { drafts: [] }) },
      processor: { flushPendingAcks: async () => 0, process: async () => {} },
      getConfig: async () => ({ ...cfg, apiToken: "" }),
      sleep: noSleep,
    });
    assert.equal(await poller.runOnce(), "not_configured");
    assert.equal(called, false);
  });

  test("instant empty responses are rate limited (no busy loop)", async () => {
    const sleep = recordingSleep();
    const poller = new DraftPoller({
      api: { pendingDrafts: async () => ({ drafts: [] }) },
      processor: { flushPendingAcks: async () => 0, process: async () => {} },
      getConfig: async () => cfg,
      sleep,
      now: () => 0,
    });
    assert.equal(await poller.runOnce(), "empty");
    assert.deepEqual(sleep.delays, [1000]);
  });

  test("loop is sequential: a new poll starts only after the previous one finished", async () => {
    let inFlight = 0;
    let maxInFlight = 0;
    let polls = 0;
    let poller;
    const api = {
      pendingDrafts: async () => {
        inFlight++;
        maxInFlight = Math.max(maxInFlight, inFlight);
        await new Promise((r) => setTimeout(r, 2));
        inFlight--;
        if (++polls >= 5) poller.stop();
        return { drafts: [{ draft_id: String(polls) }] };
      },
    };
    poller = new DraftPoller({
      api,
      processor: { flushPendingAcks: async () => 0, process: async () => new Promise((r) => setTimeout(r, 2)) },
      getConfig: async () => cfg,
      sleep: noSleep,
    });
    await poller.start();
    assert.equal(maxInFlight, 1);
    assert.equal(polls, 5);
  });

  test("wake() aborts the in-flight long poll", async () => {
    let aborted = false;
    let calls = 0;
    let poller;
    const api = {
      pendingDrafts: (wait, { signal }) =>
        new Promise((resolve, reject) => {
          calls++;
          if (calls === 2) {
            poller.stop();
            resolve({ drafts: [] });
            return;
          }
          signal.addEventListener("abort", () => {
            aborted = true;
            reject(new ApiError("aborted", { aborted: true }));
          });
        }),
    };
    poller = new DraftPoller({ api, processor: { flushPendingAcks: async () => 0, process: async () => {} }, getConfig: async () => cfg, sleep: noSleep });
    const loop = poller.start();
    await new Promise((r) => setTimeout(r, 5));
    poller.wake();
    await loop;
    assert.equal(aborted, true);
    assert.equal(calls, 2);
  });
});

describe("safety: the extension never sends mail", () => {
  const root = join(dirname(fileURLToPath(import.meta.url)), "..");
  const needle = "send" + "Message("; // built at runtime so this file does not match itself

  function sources(dir, out = []) {
    for (const name of readdirSync(dir)) {
      if (name === "node_modules" || name === "tests" || name.startsWith(".")) continue;
      const p = join(dir, name);
      if (statSync(p).isDirectory()) sources(p, out);
      else if (/\.(m?js|html|json)$/.test(name)) out.push(p);
    }
    return out;
  }

  test(`no source file contains ${needle}`, () => {
    const files = sources(root);
    assert.ok(files.some((f) => f.endsWith("drafts.js")), "drafts.js was scanned");
    assert.ok(files.some((f) => f.endsWith("background.js")), "background.js was scanned");
    const offenders = files.filter((f) => readFileSync(f, "utf8").includes(needle)).map((f) => relative(root, f));
    assert.deepEqual(offenders, []);
  });

  test("draft flow never touches a send API on the fake messenger", async () => {
    const messenger = fakeMessenger({ identities: { account3: plainIdentity }, messages: original });
    let sent = false;
    messenger.compose["send" + "Message"] = async () => {
      sent = true;
    };
    const { p } = makeProcessor({ messenger });
    await p.process(replyDraft());
    await p.process(newDraft());
    assert.equal(sent, false);
  });
});
