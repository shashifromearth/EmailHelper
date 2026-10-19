import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { buildEmailPayload, validateEmailPayload, MAX_BODY_TEXT_BYTES, MAX_SUBJECT_CHARS } from "../src/extract.js";

const header = {
  id: 1234,
  date: new Date("2026-10-18T09:30:00Z"),
  author: "Jane Doe <jane@acme.com>",
  subject: "Question about pricing",
  recipients: ["me@example.com"],
  ccList: [],
  headerMessageId: "CAF=abc@mail.gmail.com",
  folder: { accountId: "account3", path: "/INBOX", name: "Inbox", type: "inbox" },
  junk: false,
  read: false,
};

const full = {
  contentType: "multipart/mixed",
  headers: {
    "message-id": ["<CAF=abc@mail.gmail.com>"],
    "in-reply-to": ["<prev@example.com>"],
    references: ["<root@example.com>\r\n <prev@example.com>"],
    from: ["Jane Doe <jane@acme.com>"],
    to: ["me@example.com"],
    date: ["Sun, 18 Oct 2026 09:30:00 +0000"],
  },
  parts: [
    { contentType: "text/plain", body: "Hi, ..." },
    { contentType: "application/pdf", name: "spec.pdf", size: 48213, headers: { "content-disposition": ["attachment"] } },
  ],
};

const account = { id: "account3", name: "Work Gmail", email: "me@example.com", type: "gmail" };

describe("buildEmailPayload", () => {
  test("matches the contract example", () => {
    const p = buildEmailPayload({ header, full, account, folder: header.folder });
    assert.deepEqual(p, {
      account_id: "account3",
      account_name: "Work Gmail",
      account_email: "me@example.com",
      account_type: "gmail",
      message_id: "<CAF=abc@mail.gmail.com>",
      tb_message_id: 1234,
      folder: "INBOX",
      in_reply_to: "<prev@example.com>",
      references: ["<root@example.com>", "<prev@example.com>"],
      from: "Jane Doe <jane@acme.com>",
      to: ["me@example.com"],
      cc: [],
      subject: "Question about pricing",
      date: "2026-10-18T09:30:00Z",
      body_text: "Hi, ...",
      body_html: null,
      attachments: [{ name: "spec.pdf", content_type: "application/pdf", size: 48213 }],
    });
    assert.deepEqual(validateEmailPayload(p), []);
  });

  test("prefers listAttachments results when provided", () => {
    const p = buildEmailPayload({
      header,
      full,
      account,
      attachments: [{ name: "a.png", contentType: "image/png; name=a.png", size: 10, partName: "1.2" }],
    });
    assert.deepEqual(p.attachments, [{ name: "a.png", content_type: "image/png", size: 10 }]);
  });

  test("falls back to raw headers when MessageHeader fields are missing", () => {
    const p = buildEmailPayload({
      header: { id: 7 },
      full: {
        contentType: "text/plain",
        headers: {
          "message-id": ["no-brackets@x"],
          from: ["=?UTF-8?Q?Jos=C3=A9?= <jose@x.com>"],
          to: ['"A, B" <ab@x.com>, c@x.com'],
          cc: ["d@x.com"],
          subject: ["=?UTF-8?B?SMOpbGxv?="],
          date: ["Sun, 18 Oct 2026 11:30:00 +0200"],
        },
        body: "text",
      },
      account: { id: "account1", type: "imap" },
    });
    assert.equal(p.message_id, "<no-brackets@x>");
    assert.equal(p.from, "José <jose@x.com>");
    assert.deepEqual(p.to, ['"A, B" <ab@x.com>', "c@x.com"]);
    assert.deepEqual(p.cc, ["d@x.com"]);
    assert.equal(p.subject, "Héllo");
    assert.equal(p.date, "2026-10-18T09:30:00Z");
    assert.equal(p.in_reply_to, null);
    assert.deepEqual(p.references, []);
    assert.equal(p.folder, null);
    assert.deepEqual(validateEmailPayload(p), []);
  });

  test("uses headerMessageId when getFull has no Message-ID", () => {
    const p = buildEmailPayload({ header, full: { contentType: "text/plain", body: "" }, account });
    assert.equal(p.message_id, "<CAF=abc@mail.gmail.com>");
    assert.equal(p.body_text, "");
    assert.deepEqual(validateEmailPayload(p), []);
  });

  test("truncates body_text to 64 KiB and subject to 998 chars", () => {
    const big = "é".repeat(40000); // 80000 bytes
    const p = buildEmailPayload({
      header: { ...header, subject: "s".repeat(2000) },
      full: { contentType: "text/plain", body: big },
      account,
    });
    assert.ok(new TextEncoder().encode(p.body_text).length <= MAX_BODY_TEXT_BYTES);
    assert.equal(p.body_text.length, MAX_BODY_TEXT_BYTES / 2);
    assert.equal(p.subject.length, MAX_SUBJECT_CHARS);
    assert.deepEqual(validateEmailPayload(p), []);
  });

  test("unknown account type becomes 'other'", () => {
    const p = buildEmailPayload({ header, full, account: { ...account, type: "exchange" } });
    assert.equal(p.account_type, "other");
  });
});

describe("validateEmailPayload", () => {
  const good = () => buildEmailPayload({ header, full, account });

  test("required fields", () => {
    const p = good();
    delete p.account_id;
    p.message_id = "";
    p.tb_message_id = "1234";
    p.from = "";
    p.subject = null;
    p.body_text = undefined;
    const errs = validateEmailPayload(p);
    assert.deepEqual(errs, [
      "account_id is required",
      "message_id is required",
      "from is required",
      "tb_message_id must be a non-negative integer",
      "subject must be a string",
      "body_text must be a string",
    ]);
  });

  test("empty subject and body are allowed", () => {
    const p = { ...good(), subject: "", body_text: "" };
    assert.deepEqual(validateEmailPayload(p), []);
  });

  test("type checks for optional fields", () => {
    const p = { ...good(), account_type: "exchange", to: "me@x", references: [1], date: "18 Oct 2026", attachments: [{ name: "x" }] };
    const errs = validateEmailPayload(p);
    assert.equal(errs.length, 5);
    assert.ok(errs.some((e) => e.startsWith("account_type")));
    assert.ok(errs.includes("to must be an array of strings"));
    assert.ok(errs.includes("references must be an array of strings"));
    assert.ok(errs.includes("date must be an ISO 8601 UTC timestamp"));
    assert.ok(errs.includes("attachments entries need name, content_type, size"));
  });

  test("oversized fields are rejected", () => {
    const p = { ...good(), subject: "x".repeat(999), body_text: "x".repeat(MAX_BODY_TEXT_BYTES + 1) };
    assert.deepEqual(validateEmailPayload(p), ["subject exceeds 998 characters", "body_text exceeds 64 KiB"]);
    assert.deepEqual(validateEmailPayload(null), ["payload must be an object"]);
  });
});
