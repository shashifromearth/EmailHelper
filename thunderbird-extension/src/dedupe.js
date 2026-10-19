// Client-side duplicate suppression keyed by `accountId|<message-id>`.
// The backend dedupes too; this just avoids re-reading/re-posting mail that
// Thunderbird reports twice (e.g. after a folder resync or restart).

export const SEEN_KEY = "ehSeen";
export const DEFAULT_CAPACITY = 5000;

export function dedupeKey(accountId, messageId) {
  return `${accountId}|${String(messageId || "").trim()}`;
}

/** Insertion-ordered set with LRU eviction (`add` refreshes recency). */
export class LruSet {
  constructor(capacity = DEFAULT_CAPACITY, items = []) {
    this.capacity = Math.max(1, capacity);
    this.map = new Map();
    for (const k of items) this.add(k);
  }

  get size() {
    return this.map.size;
  }

  has(key) {
    return this.map.has(key);
  }

  add(key) {
    if (this.map.has(key)) this.map.delete(key);
    this.map.set(key, true);
    while (this.map.size > this.capacity) {
      this.map.delete(this.map.keys().next().value);
    }
    return this;
  }

  delete(key) {
    return this.map.delete(key);
  }

  toArray() {
    return [...this.map.keys()];
  }
}

/** LruSet persisted to storage.local (writes are debounced). */
export class DedupeStore {
  constructor({ storage, key = SEEN_KEY, capacity = DEFAULT_CAPACITY, debounceMs = 500 } = {}) {
    this.storage = storage;
    this.key = key;
    this.debounceMs = debounceMs;
    this.set = new LruSet(capacity);
    this._timer = null;
    this._dirty = false;
  }

  async load() {
    const got = await this.storage.get(this.key);
    const arr = got && Array.isArray(got[this.key]) ? got[this.key] : [];
    this.set = new LruSet(this.set.capacity, arr.filter((k) => typeof k === "string"));
    return this;
  }

  has(key) {
    return this.set.has(key);
  }

  get size() {
    return this.set.size;
  }

  add(key) {
    this.set.add(key);
    this._dirty = true;
    if (this.debounceMs <= 0) return this.flush();
    if (!this._timer) {
      this._timer = setTimeout(() => {
        this._timer = null;
        this.flush().catch(() => {});
      }, this.debounceMs);
    }
    return Promise.resolve();
  }

  async flush() {
    if (this._timer) {
      clearTimeout(this._timer);
      this._timer = null;
    }
    if (!this._dirty) return;
    this._dirty = false;
    await this.storage.set({ [this.key]: this.set.toArray() });
  }
}
