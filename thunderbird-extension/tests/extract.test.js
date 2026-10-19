import { test, describe } from "node:test";
import assert from "node:assert/strict";
import {
  extractBody,
  collectAttachments,
  htmlToText,
  parseHeaderBlock,
  parseMessageIdList,
  normalizeMessageId,
  getHeader,
  getHeaderAll,
  splitAddressList,
  decodeMimeWords,
  inferAccountType,
  shouldSkipFolder,
  getFolderAccountId,
  folderPath,
  isJunk,
  truncateUtf8,
  toIsoDate,
  decodeHtmlEntities,
} from "../src/extract.js";

describe("extractBody", () => {
  test("multipart/alternative prefers text/plain", () => {
    const full = {
      contentType: "multipart/alternative",
      parts: [
        { contentType: "text/plain", body: "Hello plain\r\nLine 2" },
        { contentType: "text/html", body: "<p>Hello <b>HTML</b></p>" },
      ],
    };
    const r = extractBody(full);
    assert.equal(r.source, "plain");
    assert.equal(r.text, "Hello plain\nLine 2");
  });

  test("multipart/alternative falls back to HTML when plain part is empty", () => {
    const full = {
      contentType: "multipart/alternative",
      parts: [
        { contentType: "text/plain", body: "   " },
        { contentType: "text/html", body: "<div>Only&nbsp;HTML</div>" },
      ],
    };
    assert.equal(extractBody(full).text, "Only HTML");
  });

  test("nested multipart/mixed with attachments", () => {
    const full = {
      contentType: "multipart/mixed",
      headers: { "content-type": ["multipart/mixed; boundary=x"] },
      parts: [
        {
          contentType: "multipart/related",
          parts: [
            {
              contentType: "multipart/alternative",
              parts: [
                { contentType: "text/plain", body: "Body text" },
                { contentType: "text/html", body: "<p>Body text</p><img src=cid:logo>" },
              ],
            },
            { contentType: "image/png", headers: { "content-disposition": ["inline"], "content-id": ["<logo>"] }, size: 1200 },
          ],
        },
        {
          contentType: "application/pdf",
          name: "spec.pdf",
          size: 48213,
          headers: { "content-disposition": ['attachment; filename="spec.pdf"'] },
        },
        {
          contentType: "text/plain",
          headers: { "content-disposition": ["attachment; filename=notes.txt"] },
          body: "attached notes, not body",
          size: 24,
        },
      ],
    };
    const body = extractBody(full);
    assert.equal(body.text, "Body text");
    const atts = collectAttachments(full);
    assert.deepEqual(atts, [
      { name: "unnamed", content_type: "image/png", size: 1200 },
      { name: "spec.pdf", content_type: "application/pdf", size: 48213 },
      { name: "notes.txt", content_type: "text/plain", size: 24 },
    ]);
  });

  test("HTML-only email is converted to text", () => {
    const full = {
      contentType: "text/html",
      body:
        "<html><head><style>p{color:red}</style><title>x</title></head><body>" +
        "<h1>Invoice</h1><p>Dear Bob,<br>Please see <a href=\"https://acme.com/inv/1\">your invoice</a>.</p>" +
        "<ul><li>One</li><li>Two &amp; three</li></ul><script>alert(1)</script>" +
        "<table><tr><td>A</td><td>B</td></tr></table><p>&copy; Acme &#8211; &#x263A;</p></body></html>",
    };
    const r = extractBody(full);
    assert.equal(r.source, "html");
    assert.equal(
      r.text,
      "Invoice\n\nDear Bob,\nPlease see your invoice (https://acme.com/inv/1).\n\n- One\n- Two & three\n\nA B\n\n© Acme – ☺",
    );
    assert.ok(!r.text.includes("alert"));
    assert.ok(!r.text.includes("color:red"));
  });

  test("forwarded message attachment is not merged into the body", () => {
    const full = {
      contentType: "multipart/mixed",
      parts: [
        { contentType: "text/plain", body: "See below" },
        {
          contentType: "message/rfc822",
          name: "fwd.eml",
          size: 999,
          headers: { "content-disposition": ["attachment"] },
          parts: [{ contentType: "text/plain", body: "inner secret" }],
        },
      ],
    };
    assert.equal(extractBody(full).text, "See below");
    assert.deepEqual(collectAttachments(full), [{ name: "fwd.eml", content_type: "message/rfc822", size: 999 }]);
  });

  test("message without text parts yields empty body", () => {
    assert.deepEqual(extractBody({ contentType: "multipart/mixed", parts: [] }), { text: "", html: null, source: "none" });
    assert.equal(extractBody(null).text, "");
  });

  test("RFC 2231 and encoded-word attachment names", () => {
    const full = {
      contentType: "multipart/mixed",
      parts: [
        { contentType: "text/plain", body: "x" },
        { contentType: "application/pdf", headers: { "content-disposition": ["attachment; filename*=utf-8''R%C3%A9sum%C3%A9.pdf"] } },
        { contentType: "image/jpeg", headers: { "content-type": ['image/jpeg; name="=?UTF-8?B?Rm90w7Mu?= =?UTF-8?Q?jpg?="'] } },
      ],
    };
    assert.deepEqual(
      collectAttachments(full).map((a) => a.name),
      ["Résumé.pdf", "Fotó.jpg"],
    );
  });
});

describe("htmlToText", () => {
  test("drops comments, mailto links keep text, collapses whitespace", () => {
    const t = htmlToText('<!-- hidden --><p>Hi   <a href="mailto:a@b.c">Ann</a></p>\n\n\n<p>   Bye</p>');
    assert.equal(t, "Hi Ann\n\nBye");
  });
  test("link with identical label is not duplicated", () => {
    assert.equal(htmlToText('<a href="https://x.io">https://x.io</a>'), "https://x.io");
  });
  test("entities", () => {
    assert.equal(decodeHtmlEntities("&lt;a&gt; &amp;amp; &unknown; &#65;"), "<a> &amp; &unknown; A");
  });
});

describe("headers", () => {
  const raw = [
    "Message-ID: <CAF=abc@mail.gmail.com>",
    "In-Reply-To:",
    " <prev@example.com>",
    "References: <root@example.com>",
    "\t<mid@example.com>",
    "   <prev@example.com>",
    "Subject: =?UTF-8?B?UXVlc3Rpb24gYWJvdXQ=?=",
    " =?UTF-8?Q?_pricing_=E2=82=AC?=",
    "To: \"Doe, Jane\" <jane@acme.com>, bob@acme.com,",
    " (team) carol@acme.com",
    "Received: from a",
    "Received: from b",
    "",
    "Body: not a header",
  ].join("\r\n");

  test("parseHeaderBlock unfolds folded headers and keeps repeated ones", () => {
    const h = parseHeaderBlock(raw);
    assert.equal(getHeader(h, "message-id"), "<CAF=abc@mail.gmail.com>");
    assert.equal(getHeader(h, "In-Reply-To"), "<prev@example.com>");
    assert.deepEqual(getHeaderAll(h, "received"), ["from a", "from b"]);
    assert.equal(h.body, undefined);
  });

  test("multiple references are parsed in order", () => {
    const h = parseHeaderBlock(raw);
    assert.deepEqual(parseMessageIdList(getHeader(h, "references")), ["<root@example.com>", "<mid@example.com>", "<prev@example.com>"]);
  });

  test("references from getFull-style multi-value arrays with folding and duplicates", () => {
    const headers = { references: ["<a@x>\r\n <b@x>,<c@x>", "<b@x> <d@x>"] };
    assert.deepEqual(parseMessageIdList(getHeaderAll(headers, "references")), ["<a@x>", "<b@x>", "<c@x>", "<d@x>"]);
  });

  test("message ids without brackets are repaired", () => {
    assert.deepEqual(parseMessageIdList("abc@x def@y"), ["<abc@x>", "<def@y>"]);
    assert.equal(normalizeMessageId("abc@x"), "<abc@x>");
    assert.equal(normalizeMessageId("  <abc@x>  "), "<abc@x>");
    assert.equal(normalizeMessageId(""), null);
  });

  test("encoded words decode across folds", () => {
    const h = parseHeaderBlock(raw);
    assert.equal(decodeMimeWords(getHeader(h, "subject")), "Question about pricing €");
    assert.equal(decodeMimeWords("=?iso-8859-1?Q?caf=E9?="), "café");
    assert.equal(decodeMimeWords("plain"), "plain");
  });

  test("address lists split on top-level commas only", () => {
    const h = parseHeaderBlock(raw);
    assert.deepEqual(splitAddressList(getHeader(h, "to")), ['"Doe, Jane" <jane@acme.com>', "bob@acme.com", "(team) carol@acme.com"]);
    assert.deepEqual(splitAddressList(null), []);
  });

  test("dates normalise to second-precision UTC", () => {
    assert.equal(toIsoDate(new Date("2026-10-18T09:30:00.123Z")), "2026-10-18T09:30:00Z");
    assert.equal(toIsoDate("Sun, 18 Oct 2026 11:30:00 +0200"), "2026-10-18T09:30:00Z");
    assert.equal(toIsoDate("garbage"), null);
    assert.equal(toIsoDate(null), null);
  });
});

describe("accounts and folders", () => {
  test("account type inference from hostname", () => {
    assert.equal(inferAccountType({ hostname: "imap.gmail.com", type: "imap" }), "gmail");
    assert.equal(inferAccountType({ hostname: "imap.googlemail.com", type: "imap" }), "gmail");
    assert.equal(inferAccountType({ hostname: "outlook.office365.com", type: "imap" }), "outlook");
    assert.equal(inferAccountType({ hostname: "imap-mail.outlook.com", type: "imap" }), "outlook");
    assert.equal(inferAccountType({ hostname: "imap.zoho.eu", type: "imap" }), "zoho");
    assert.equal(inferAccountType({ hostname: "imappro.zoho.com", type: "imap" }), "zoho");
    assert.equal(inferAccountType({ hostname: "mail.example.org", type: "imap" }), "imap");
    assert.equal(inferAccountType({ hostname: "pop.example.org", type: "pop3" }), "pop3");
  });

  test("account type inference from identity e-mail and name", () => {
    assert.equal(inferAccountType({ type: "imap", emails: ["me@hotmail.com"] }), "outlook");
    assert.equal(inferAccountType({ type: "imap", emails: ["me@live.co.uk"] }), "outlook");
    assert.equal(inferAccountType({ type: "imap", emails: ["Me <me@outlook.de>"] }), "outlook");
    assert.equal(inferAccountType({ type: "imap", emails: ["me@zohomail.in"] }), "zoho");
    assert.equal(inferAccountType({ type: "imap", name: "me@gmail.com" }), "gmail");
    assert.equal(inferAccountType({ type: "pop3", emails: ["me@corp.example"] }), "pop3");
    assert.equal(inferAccountType({ type: "none" }), "other");
    assert.equal(inferAccountType({ type: "nntp" }), "other");
    assert.equal(inferAccountType({ type: "ews", emails: ["me@corp.example"] }), "outlook");
    // Lookalikes must not match
    assert.equal(inferAccountType({ type: "imap", emails: ["me@live.example.org"] }), "imap");
    assert.equal(inferAccountType({ type: "imap", emails: ["me@notgmail.com"] }), "imap");
  });

  test("folder skip rules (TB115 type, TB128 specialUse, names)", () => {
    assert.equal(shouldSkipFolder({ type: "inbox", path: "/INBOX" }), false);
    for (const type of ["junk", "trash", "sent", "drafts", "templates", "outbox"]) assert.equal(shouldSkipFolder({ type, path: "/x" }), true, type);
    assert.equal(shouldSkipFolder({ specialUse: ["junk"], path: "/x" }), true);
    assert.equal(shouldSkipFolder({ specialUse: ["inbox"], path: "/INBOX" }), false);
    assert.equal(shouldSkipFolder({ path: "/[Gmail]/Spam" }), true);
    assert.equal(shouldSkipFolder({ path: "/Sent Items" }), true);
    assert.equal(shouldSkipFolder({ path: "/Projects/Acme" }), false);
    assert.equal(shouldSkipFolder(null), true);
  });

  test("folder account id across TB versions", () => {
    assert.equal(getFolderAccountId({ accountId: "account3", path: "/INBOX" }), "account3");
    assert.equal(getFolderAccountId({ id: "account7://INBOX", path: "/INBOX" }), "account7");
    assert.equal(getFolderAccountId({}), null);
    assert.equal(folderPath({ path: "/INBOX", name: "Inbox" }), "INBOX");
    assert.equal(folderPath({ path: "/", name: "Root" }), "Root");
  });

  test("junk detection", () => {
    assert.equal(isJunk({ junk: true }), true);
    assert.equal(isJunk({ junk: false, junkScore: 100 }), true);
    assert.equal(isJunk({ junk: false, junkScore: 0 }), false);
  });
});

describe("truncateUtf8", () => {
  test("never splits a multi-byte character", () => {
    const s = "a" + "€".repeat(10); // 1 + 30 bytes
    const t = truncateUtf8(s, 8);
    assert.equal(t, "a€€");
    assert.ok(new TextEncoder().encode(t).length <= 8);
    assert.equal(truncateUtf8("short", 64), "short");
  });
  test("emoji (4-byte) boundary", () => {
    const t = truncateUtf8("\u{1F600}\u{1F600}", 6);
    assert.equal(t, "\u{1F600}");
  });
});
