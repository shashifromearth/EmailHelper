// Shared fakes for the tests (no external deps).

export class MemoryStorage {
  constructor(initial = {}) {
    this.data = structuredClone(initial);
    this.writes = 0;
  }
  async get(keys) {
    const list = keys === null || keys === undefined ? Object.keys(this.data) : Array.isArray(keys) ? keys : [keys];
    const out = {};
    for (const k of list) if (k in this.data) out[k] = structuredClone(this.data[k]);
    return out;
  }
  async set(obj) {
    this.writes++;
    for (const [k, v] of Object.entries(obj)) this.data[k] = structuredClone(v);
  }
}

/** fetch() stand-in. `handler(url, init)` returns {status, body} or throws. */
export function fakeFetch(handler) {
  const calls = [];
  const fn = async (url, init = {}) => {
    calls.push({ url, init, body: init.body ? JSON.parse(init.body) : undefined });
    const r = await handler(url, init, calls.length);
    const text = r.body === undefined || r.body === null ? "" : typeof r.body === "string" ? r.body : JSON.stringify(r.body);
    return {
      status: r.status,
      ok: r.status >= 200 && r.status < 300,
      text: async () => text,
    };
  };
  fn.calls = calls;
  return fn;
}

export const networkDown = () => {
  throw new TypeError("NetworkError when attempting to fetch resource.");
};

export const noSleep = () => Promise.resolve();

export function recordingSleep() {
  const delays = [];
  const fn = async (ms) => {
    delays.push(ms);
  };
  fn.delays = delays;
  return fn;
}

/** Minimal fake of the parts of `messenger` used by drafts.js / ingest.js. */
export function fakeMessenger({ identities = {}, messages = {}, saveResult = { mode: "draft", messages: [{ id: 5678 }] }, saveError = null, composeIsPlainText = null } = {}) {
  const calls = [];
  let tabSeq = 100;
  const rec = (name, ...args) => calls.push({ name, args });
  const m = {
    calls,
    identities: {
      async getDefault(accountId) {
        rec("identities.getDefault", accountId);
        return identities[accountId] || null;
      },
    },
    accounts: {
      async get(accountId) {
        rec("accounts.get", accountId);
        return { id: accountId, name: `Account ${accountId}`, type: "imap", identities: identities[accountId] ? [identities[accountId]] : [] };
      },
    },
    messages: {
      async get(id) {
        rec("messages.get", id);
        if (!messages[id]) throw new Error(`message ${id} not found`);
        return messages[id];
      },
      async query(q) {
        rec("messages.query", q);
        return { id: null, messages: Object.values(messages).filter((h) => h.headerMessageId === q.headerMessageId) };
      },
    },
    compose: {
      async beginReply(id, type, details) {
        rec("compose.beginReply", id, type, details);
        return { id: ++tabSeq, windowId: tabSeq + 1000, type: "messageCompose" };
      },
      async beginNew(details) {
        rec("compose.beginNew", details);
        return { id: ++tabSeq, windowId: tabSeq + 1000, type: "messageCompose" };
      },
      async getComposeDetails(tabId) {
        rec("compose.getComposeDetails", tabId);
        return composeIsPlainText === null ? {} : { isPlainText: composeIsPlainText };
      },
      async setComposeDetails(tabId, details) {
        rec("compose.setComposeDetails", tabId, details);
      },
      async saveMessage(tabId, opts) {
        rec("compose.saveMessage", tabId, opts);
        if (saveError) throw new Error(saveError);
        return saveResult;
      },
    },
    windows: {
      async remove(windowId) {
        rec("windows.remove", windowId);
      },
    },
    tabs: {
      async remove(tabId) {
        rec("tabs.remove", tabId);
      },
    },
  };
  m.names = () => calls.map((c) => c.name);
  m.find = (name) => calls.filter((c) => c.name === name);
  return m;
}
