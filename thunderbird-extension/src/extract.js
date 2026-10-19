// Pure helpers that turn Thunderbird MailExtension data (MessageHeader,
// MessagePart from messages.getFull, MailFolder, MailAccount) into the
// POST /api/v1/emails payload defined in documents/API.md section 1.
//
// Nothing in this module touches `messenger.*`, so it is unit tested in Node.

export const MAX_BODY_TEXT_BYTES = 64 * 1024;
export const MAX_SUBJECT_CHARS = 998;
export const MAX_REFERENCES = 50;
export const MAX_RECIPIENTS = 200;
export const MAX_ATTACHMENTS = 100;
export const ACCOUNT_TYPES = Object.freeze(["gmail", "outlook", "zoho", "imap", "pop3", "other"]);

// ---------------------------------------------------------------------------
// Folders, junk, accounts
// ---------------------------------------------------------------------------

const SKIP_FOLDER_TYPES = new Set(["junk", "trash", "sent", "drafts", "templates", "outbox"]);
const SKIP_FOLDER_NAMES = new Set([
  "junk", "junk e-mail", "junk email", "spam", "bulk mail",
  "trash", "bin", "deleted items", "deleted messages",
  "sent", "sent items", "sent mail", "sent messages",
  "drafts", "templates", "outbox", "unsent messages",
]);

/**
 * True if mail in this folder must not be processed. Handles the TB115 shape
 * (`folder.type`), the TB121+/128 shape (`folder.specialUse` array) and falls
 * back to well-known folder names when the server did not flag the folder.
 */
export function shouldSkipFolder(folder) {
  if (!folder || typeof folder !== "object") return true;
  if (typeof folder.type === "string" && SKIP_FOLDER_TYPES.has(folder.type.toLowerCase())) return true;
  if (Array.isArray(folder.specialUse)) {
    for (const use of folder.specialUse) {
      if (SKIP_FOLDER_TYPES.has(String(use).toLowerCase())) return true;
    }
  }
  const path = typeof folder.path === "string" ? folder.path : "";
  const last = (path.split("/").filter(Boolean).pop() || folder.name || "").toLowerCase();
  return SKIP_FOLDER_NAMES.has(last);
}

/** Account id of a MailFolder: TB115 `accountId`, or derived from TB128 `id` ("account1://INBOX"). */
export function getFolderAccountId(folder) {
  if (!folder || typeof folder !== "object") return null;
  if (typeof folder.accountId === "string" && folder.accountId) return folder.accountId;
  if (folder.account && typeof folder.account.id === "string") return folder.account.id;
  if (typeof folder.id === "string") {
    const idx = folder.id.indexOf("://");
    if (idx > 0) return folder.id.slice(0, idx);
  }
  return null;
}

/** Folder name as sent to the backend: "/INBOX" -> "INBOX". */
export function folderPath(folder) {
  if (!folder || typeof folder !== "object") return null;
  if (typeof folder.path === "string") {
    const p = folder.path.replace(/^\/+/, "");
    if (p) return p;
  }
  return typeof folder.name === "string" && folder.name ? folder.name : null;
}

export function isJunk(header) {
  if (!header) return false;
  if (header.junk === true) return true;
  return typeof header.junkScore === "number" && header.junkScore >= 100;
}

const TLD = String.raw`(?:com|net|org|[a-z]{2}|co\.[a-z]{2}|com\.[a-z]{2})`;
const PROVIDER_PATTERNS = [
  ["gmail", new RegExp(String.raw`(^|\.)(gmail|googlemail)\.${TLD}$`)],
  ["outlook", new RegExp(String.raw`(^|\.)(outlook|office365|hotmail|live|msn)\.${TLD}$`)],
  ["zoho", new RegExp(String.raw`(^|\.)(zoho|zohomail)\.${TLD}$`)],
];

function domainOf(value) {
  if (typeof value !== "string") return "";
  let v = value.trim().toLowerCase();
  const lt = v.lastIndexOf("<");
  if (lt >= 0) v = v.slice(lt + 1).replace(/>.*$/, "");
  const at = v.lastIndexOf("@");
  if (at >= 0) v = v.slice(at + 1);
  return v.replace(/[:/].*$/, "").replace(/\.$/, "");
}

function matchProvider(value) {
  const d = domainOf(value);
  if (!d) return null;
  for (const [type, re] of PROVIDER_PATTERNS) if (re.test(d)) return type;
  return null;
}

/**
 * Infer the contract's `account_type`.
 *
 * The MailExtension API does not expose the incoming server hostname, so the
 * hostname is used when a caller can supply it, then identity e-mail domains,
 * then the account name (Thunderbird defaults it to the e-mail address), and
 * finally the protocol from `account.type`.
 */
export function inferAccountType({ hostname = null, type = null, emails = [], name = null } = {}) {
  const candidates = [hostname, ...(Array.isArray(emails) ? emails : []), name];
  for (const c of candidates) {
    const t = matchProvider(c);
    if (t) return t;
  }
  const proto = typeof type === "string" ? type.toLowerCase() : "";
  if (proto === "imap") return "imap";
  if (proto === "pop3") return "pop3";
  if (proto === "ews" || proto === "owl" || proto === "graph") return "outlook";
  return "other";
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

export function unfoldHeader(value) {
  if (value === null || value === undefined) return "";
  return String(value).replace(/\r?\n[ \t]+/g, " ").replace(/[\r\n]+/g, " ").trim();
}

/** Parse a raw RFC 5322 header block into `{ lowercased-name: [values] }` (same shape as getFull). */
export function parseHeaderBlock(raw) {
  const out = {};
  if (typeof raw !== "string") return out;
  const lines = raw.replace(/\r\n/g, "\n").split("\n");
  let current = null;
  for (const line of lines) {
    if (line === "") break; // end of header section
    if (/^[ \t]/.test(line) && current) {
      current.value += "\n" + line;
      continue;
    }
    const idx = line.indexOf(":");
    if (idx <= 0) continue;
    if (current) (out[current.name] ||= []).push(unfoldHeader(current.value));
    current = { name: line.slice(0, idx).trim().toLowerCase(), value: line.slice(idx + 1) };
  }
  if (current) (out[current.name] ||= []).push(unfoldHeader(current.value));
  return out;
}

export function getHeaderAll(headers, name) {
  if (!headers || typeof headers !== "object") return [];
  const key = name.toLowerCase();
  let v = headers[key];
  if (v === undefined) {
    const found = Object.keys(headers).find((k) => k.toLowerCase() === key);
    v = found === undefined ? undefined : headers[found];
  }
  if (v === undefined || v === null) return [];
  return (Array.isArray(v) ? v : [v]).map(unfoldHeader).filter((s) => s !== "");
}

export function getHeader(headers, name) {
  const all = getHeaderAll(headers, name);
  return all.length ? all[0] : null;
}

/** Extract `<id>` tokens from In-Reply-To / References (handles folding, commas, junk text). */
export function parseMessageIdList(value) {
  if (value === null || value === undefined) return [];
  const text = unfoldHeader(Array.isArray(value) ? value.join(" ") : value);
  const out = [];
  const seen = new Set();
  const push = (id) => {
    if (!seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  };
  const bracketed = text.match(/<[^<>\s]+>/g);
  if (bracketed) {
    bracketed.forEach(push);
    return out;
  }
  // Broken mailers sometimes omit the brackets.
  for (const tok of text.split(/[\s,]+/)) {
    if (tok.includes("@")) push(`<${tok}>`);
  }
  return out;
}

export function normalizeMessageId(value) {
  if (value === null || value === undefined) return null;
  const ids = parseMessageIdList(value);
  if (ids.length) return ids[0];
  const t = unfoldHeader(value).replace(/^<|>$/g, "");
  return t ? `<${t}>` : null;
}

export function stripAngleBrackets(value) {
  return typeof value === "string" ? value.trim().replace(/^<|>$/g, "") : "";
}

function bytesFromBinaryString(s) {
  const bytes = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i) & 0xff;
  return bytes;
}

function decodeBytes(bytes, charset) {
  try {
    return new TextDecoder(charset || "utf-8").decode(bytes);
  } catch {
    return String.fromCharCode(...bytes); // unknown charset: latin1
  }
}

/** Decode RFC 2047 encoded-words (`=?UTF-8?B?...?=`, `=?iso-8859-1?Q?...?=`). */
export function decodeMimeWords(value) {
  if (typeof value !== "string" || !value.includes("=?")) return value ?? "";
  const joined = value.replace(/(\?=)\s+(?==\?)/g, "$1");
  return joined.replace(/=\?([^?\s]+)\?([bBqQ])\?([^?\s]*)\?=/g, (m, charset, enc, text) => {
    try {
      const cs = charset.split("*")[0];
      let binary;
      if (enc.toUpperCase() === "B") {
        binary = atob(text);
      } else {
        binary = text.replace(/_/g, " ").replace(/=([0-9A-Fa-f]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16)));
      }
      return decodeBytes(bytesFromBinaryString(binary), cs);
    } catch {
      return m;
    }
  });
}

/** Split an address list header on top-level commas (respects quotes, <>, comments). */
export function splitAddressList(value) {
  if (value === null || value === undefined) return [];
  const text = decodeMimeWords(unfoldHeader(value));
  const out = [];
  let cur = "";
  let inQuote = false;
  let angle = 0;
  let paren = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\" && inQuote && i + 1 < text.length) {
      cur += ch + text[++i];
      continue;
    }
    if (ch === '"' && paren === 0) inQuote = !inQuote;
    else if (!inQuote) {
      if (ch === "<") angle++;
      else if (ch === ">" && angle > 0) angle--;
      else if (ch === "(") paren++;
      else if (ch === ")" && paren > 0) paren--;
      else if (ch === "," && angle === 0 && paren === 0) {
        if (cur.trim()) out.push(cur.trim());
        cur = "";
        continue;
      }
    }
    cur += ch;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}

/** Date -> "2026-10-18T09:30:00Z" (UTC, second precision), or null. */
export function toIsoDate(value) {
  if (value === null || value === undefined || value === "") return null;
  let d;
  if (value instanceof Date) d = value;
  else if (typeof value === "number") d = new Date(value);
  else d = new Date(Date.parse(String(value)));
  if (Number.isNaN(d.getTime())) return null;
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}

// ---------------------------------------------------------------------------
// HTML -> text
// ---------------------------------------------------------------------------

const NAMED_ENTITIES = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ",
  copy: "©", reg: "®", trade: "™", hellip: "…",
  mdash: "—", ndash: "–", lsquo: "‘", rsquo: "’",
  ldquo: "“", rdquo: "”", sbquo: "‚", bdquo: "„",
  bull: "•", middot: "·", euro: "€", pound: "£",
  yen: "¥", cent: "¢", laquo: "«", raquo: "»",
  deg: "°", times: "×", divide: "÷", para: "¶",
  sect: "§", shy: "", zwnj: "", zwj: "", thinsp: " ",
  ensp: " ", emsp: " ", iexcl: "¡", iquest: "¿",
};

export function decodeHtmlEntities(text) {
  return String(text).replace(/&(#\d+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);/g, (m, ent) => {
    if (ent[0] === "#") {
      const code = ent[1] === "x" || ent[1] === "X" ? parseInt(ent.slice(2), 16) : parseInt(ent.slice(1), 10);
      if (!Number.isFinite(code) || code < 0 || code > 0x10ffff) return m;
      try {
        return String.fromCodePoint(code);
      } catch {
        return m;
      }
    }
    const v = NAMED_ENTITIES[ent] ?? NAMED_ENTITIES[ent.toLowerCase()];
    return v === undefined ? m : v;
  });
}

const BLOCK_TAGS = "p|div|h[1-6]|tr|table|thead|tbody|ul|ol|dl|dt|dd|blockquote|pre|section|article|header|footer|address|figure|center";

export function htmlToText(html) {
  if (typeof html !== "string" || !html) return "";
  let s = html;
  s = s.replace(/<!--[\s\S]*?-->/g, "");
  s = s.replace(/<(script|style|head|title|noscript|template|svg)\b[^>]*>[\s\S]*?<\/\1\s*>/gi, "");
  s = s.replace(/<a\b[^>]*?\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))[^>]*>([\s\S]*?)<\/a\s*>/gi, (m, h1, h2, h3, inner) => {
    const href = decodeHtmlEntities((h1 ?? h2 ?? h3 ?? "").trim());
    const label = decodeHtmlEntities(inner.replace(/<[^>]*>/g, "")).replace(/\s+/g, " ").trim();
    if (!href || /^(mailto:|javascript:|#)/i.test(href) || label === href || !label) return inner || href;
    return `${inner} (${href})`;
  });
  s = s.replace(/<br\s*\/?>/gi, "\n");
  s = s.replace(/<hr\b[^>]*>/gi, "\n----\n");
  s = s.replace(/<li\b[^>]*>/gi, "\n- ");
  s = s.replace(new RegExp(`</(?:${BLOCK_TAGS})\\s*>`, "gi"), "\n");
  s = s.replace(new RegExp(`<(?:${BLOCK_TAGS})\\b[^>]*>`, "gi"), "\n");
  s = s.replace(/<\/t[dh]\s*>/gi, "\t");
  s = s.replace(/<[^>]*>/g, "");
  s = decodeHtmlEntities(s);
  s = s
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => line.replace(/[ \t \f\v]+/g, " ").trim())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n");
  return s.trim();
}

// ---------------------------------------------------------------------------
// MIME tree (messages.getFull)
// ---------------------------------------------------------------------------

function headerParam(value, param) {
  if (!value) return null;
  const re = new RegExp(`(?:^|;)\\s*${param}\\*?\\s*=\\s*(?:"((?:[^"\\\\]|\\\\.)*)"|([^;\\s]+))`, "i");
  const m = re.exec(value);
  if (!m) return null;
  let v = m[1] !== undefined ? m[1].replace(/\\(.)/g, "$1") : m[2];
  // RFC 2231: utf-8''na%20me
  const rfc2231 = /^([\w-]+)'[^']*'(.*)$/.exec(v);
  if (rfc2231) {
    try {
      v = decodeURIComponent(rfc2231[2]);
    } catch {
      /* keep raw */
    }
  }
  return decodeMimeWords(v);
}

function partContentType(part) {
  let ct = typeof part.contentType === "string" ? part.contentType : "";
  if (!ct) ct = getHeader(part.headers, "content-type") || "";
  return (ct.split(";")[0] || "").trim().toLowerCase() || "text/plain";
}

function partFilename(part) {
  if (typeof part.name === "string" && part.name) return part.name;
  return (
    headerParam(getHeader(part.headers, "content-disposition"), "filename") ||
    headerParam(getHeader(part.headers, "content-type"), "name") ||
    null
  );
}

function partDisposition(part) {
  const d = getHeader(part.headers, "content-disposition");
  return d ? d.split(";")[0].trim().toLowerCase() : "";
}

function classifyLeaf(part) {
  const ct = partContentType(part);
  const disposition = partDisposition(part);
  const filename = partFilename(part);
  const isText = ct === "text/plain" || ct === "text/html";
  let attachment;
  if (disposition === "attachment") attachment = true;
  else if (filename) attachment = !(isText && disposition === "inline");
  else attachment = !isText;
  return { ct, filename, attachment, isText };
}

function bodyString(part) {
  return typeof part.body === "string" ? part.body.replace(/\r\n?/g, "\n") : "";
}

function collectBodies(part) {
  const result = { plain: [], html: [] };
  if (!part || typeof part !== "object") return result;
  const ct = partContentType(part);
  const children = Array.isArray(part.parts) ? part.parts : [];
  if (ct.startsWith("multipart/") || (children.length && !ct.startsWith("text/"))) {
    if (ct === "message/rfc822" && partDisposition(part) === "attachment") return result;
    const childResults = children.map(collectBodies);
    if (ct === "multipart/alternative") {
      const withPlain = childResults.find((r) => r.plain.some((t) => t.trim()));
      const withHtml = [...childResults].reverse().find((r) => r.html.some((t) => t.trim()));
      if (withPlain) result.plain.push(...withPlain.plain);
      if (withHtml) result.html.push(...withHtml.html);
    } else {
      for (const r of childResults) {
        result.plain.push(...r.plain);
        result.html.push(...r.html);
      }
    }
    return result;
  }
  const leaf = classifyLeaf(part);
  if (leaf.attachment || !leaf.isText) return result;
  (leaf.ct === "text/html" ? result.html : result.plain).push(bodyString(part));
  return result;
}

/**
 * Pick the message body: text/plain parts are preferred; HTML is converted to
 * text only when no non-empty text/plain part exists.
 */
export function extractBody(full) {
  const { plain, html } = collectBodies(full);
  const plainText = plain.filter((t) => t.trim()).join("\n\n").trim();
  const htmlRaw = html.filter((t) => t.trim()).join("\n");
  if (plainText) return { text: plainText, html: htmlRaw || null, source: "plain" };
  if (htmlRaw) return { text: htmlToText(htmlRaw), html: htmlRaw, source: "html" };
  return { text: "", html: null, source: "none" };
}

/** Attachment metadata (no content) from the getFull part tree. */
export function collectAttachments(full) {
  const out = [];
  const visit = (part) => {
    if (!part || typeof part !== "object") return;
    const ct = partContentType(part);
    const children = Array.isArray(part.parts) ? part.parts : [];
    if (ct.startsWith("multipart/") || (children.length && !ct.startsWith("text/") && ct !== "message/rfc822")) {
      children.forEach(visit);
      return;
    }
    const leaf = classifyLeaf(part);
    if (leaf.attachment) {
      out.push({
        name: leaf.filename || "unnamed",
        content_type: ct,
        size: Number.isFinite(part.size) ? part.size : bodyString(part).length,
      });
      return; // do not descend into attached messages
    }
    children.forEach(visit);
  };
  visit(full);
  return out;
}

/** Normalize `messages.listAttachments()` results (or already-normalized entries). */
export function normalizeAttachments(list) {
  if (!Array.isArray(list)) return [];
  return list
    .filter((a) => a && typeof a === "object")
    .map((a) => ({
      name: String(a.name || "unnamed"),
      content_type: String(a.contentType || a.content_type || "application/octet-stream").split(";")[0].trim().toLowerCase(),
      size: Number.isFinite(a.size) ? a.size : 0,
    }))
    .slice(0, MAX_ATTACHMENTS);
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

/** Truncate to at most `maxBytes` UTF-8 bytes without splitting a code point. */
export function truncateUtf8(text, maxBytes = MAX_BODY_TEXT_BYTES) {
  if (typeof text !== "string") return "";
  const enc = new TextEncoder();
  const bytes = enc.encode(text);
  if (bytes.length <= maxBytes) return text;
  let end = maxBytes;
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--; // back up to a lead byte
  return new TextDecoder().decode(bytes.subarray(0, end));
}

export function truncateChars(text, max) {
  if (typeof text !== "string") return "";
  const chars = Array.from(text);
  return chars.length <= max ? text : chars.slice(0, max).join("");
}

function asStringArray(v) {
  if (!Array.isArray(v)) return null;
  return v.filter((s) => typeof s === "string" && s.trim()).map((s) => s.trim());
}

/**
 * Build the POST /api/v1/emails body.
 *
 * @param {object} p
 * @param {object} p.header      MessageHeader (messages.get / onNewMailReceived)
 * @param {object} p.full        MessagePart (messages.getFull)
 * @param {Array}  [p.attachments] messages.listAttachments() result (preferred over the tree walk)
 * @param {object} p.account     { id, name, email, type }
 * @param {object} [p.folder]    MailFolder
 */
export function buildEmailPayload({ header, full, attachments = null, account, folder = null }) {
  const h = header || {};
  const headers = (full && full.headers) || {};

  const messageId =
    normalizeMessageId(getHeader(headers, "message-id")) ||
    (h.headerMessageId ? normalizeMessageId(h.headerMessageId) : null);

  const inReplyTo = parseMessageIdList(getHeader(headers, "in-reply-to"))[0] || null;
  let references = parseMessageIdList(getHeaderAll(headers, "references"));
  if (references.length > MAX_REFERENCES) references = references.slice(-MAX_REFERENCES);

  const from =
    (typeof h.author === "string" && h.author.trim()) ||
    decodeMimeWords(getHeader(headers, "from") || "") ||
    decodeMimeWords(getHeader(headers, "sender") || "") ||
    "";

  const to = (asStringArray(h.recipients) ?? splitAddressList(getHeader(headers, "to"))).slice(0, MAX_RECIPIENTS);
  const cc = (asStringArray(h.ccList) ?? splitAddressList(getHeader(headers, "cc"))).slice(0, MAX_RECIPIENTS);

  const subject = truncateChars(
    typeof h.subject === "string" ? h.subject : decodeMimeWords(getHeader(headers, "subject") || ""),
    MAX_SUBJECT_CHARS,
  );

  const body = extractBody(full);
  const atts = attachments ? normalizeAttachments(attachments) : collectAttachments(full).slice(0, MAX_ATTACHMENTS);

  return {
    account_id: account?.id ?? null,
    account_name: account?.name ?? null,
    account_email: account?.email ?? null,
    account_type: ACCOUNT_TYPES.includes(account?.type) ? account.type : "other",
    message_id: messageId,
    tb_message_id: h.id,
    folder: folderPath(folder || h.folder),
    in_reply_to: inReplyTo,
    references,
    from: from.trim(),
    to,
    cc,
    subject,
    date: toIsoDate(h.date) || toIsoDate(getHeader(headers, "date")),
    body_text: truncateUtf8(body.text, MAX_BODY_TEXT_BYTES),
    body_html: null,
    attachments: atts,
  };
}

/** Client-side validation mirroring the contract. Returns a list of problems (empty = valid). */
export function validateEmailPayload(p) {
  const errors = [];
  if (!p || typeof p !== "object") return ["payload must be an object"];
  const nonEmpty = (k) => {
    if (typeof p[k] !== "string" || !p[k].trim()) errors.push(`${k} is required`);
  };
  nonEmpty("account_id");
  nonEmpty("message_id");
  nonEmpty("from");
  if (!Number.isInteger(p.tb_message_id) || p.tb_message_id < 0) errors.push("tb_message_id must be a non-negative integer");
  if (typeof p.subject !== "string") errors.push("subject must be a string");
  else if (Array.from(p.subject).length > MAX_SUBJECT_CHARS) errors.push(`subject exceeds ${MAX_SUBJECT_CHARS} characters`);
  if (typeof p.body_text !== "string") errors.push("body_text must be a string");
  else if (new TextEncoder().encode(p.body_text).length > MAX_BODY_TEXT_BYTES) errors.push("body_text exceeds 64 KiB");
  if (p.account_type !== undefined && p.account_type !== null && !ACCOUNT_TYPES.includes(p.account_type)) {
    errors.push(`account_type must be one of ${ACCOUNT_TYPES.join(", ")}`);
  }
  for (const k of ["references", "to", "cc"]) {
    if (p[k] !== undefined && p[k] !== null && (!Array.isArray(p[k]) || p[k].some((s) => typeof s !== "string"))) {
      errors.push(`${k} must be an array of strings`);
    }
  }
  if (p.attachments !== undefined && p.attachments !== null) {
    if (!Array.isArray(p.attachments)) errors.push("attachments must be an array");
    else if (p.attachments.some((a) => !a || typeof a.name !== "string" || typeof a.content_type !== "string" || !Number.isFinite(a.size))) {
      errors.push("attachments entries need name, content_type, size");
    }
  }
  if (p.date !== undefined && p.date !== null && !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/.test(p.date)) {
    errors.push("date must be an ISO 8601 UTC timestamp");
  }
  return errors;
}
