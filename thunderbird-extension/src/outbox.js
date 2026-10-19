// Durable outbox for POST /api/v1/emails.
//
// Every detected e-mail is first written to storage.local, then delivered in
// FIFO order. While the backend is down, items stay queued (also across
// Thunderbird restarts) and are retried on the next flush.
//
// One flush delivers items one by one; each item gets up to `attempts` tries
// with exponential backoff. If the head item still fails with a transient
// error the flush stops (backend presumed down) and the caller schedules the
// next flush. Permanent 4xx rejections are dropped so they cannot block the
// queue; 401/403 stops the flush until the token is fixed.

import { withRetry, isRetryableError, isAuthError } from "./api.js";

export const OUTBOX_KEY = "ehOutbox";

const defaultSleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 'retry' | 'auth' | 'drop' */
export function classifyDeliveryError(err) {
  if (isAuthError(err)) return "auth";
  if (isRetryableError(err)) return "retry";
  return "drop";
}

export class Outbox {
  /**
   * @param {object} o
   * @param {object} o.storage   { get, set } like messenger.storage.local
   * @param {(payload:object) => Promise<any>} o.send  throws ApiError on failure
   */
  constructor({
    storage,
    send,
    sleep = defaultSleep,
    now = () => Date.now(),
    log = () => {},
    onChange = () => {},
    maxItems = 500,
    attempts = 5,
    baseDelayMs = 1000,
    maxDelayMs = 30000,
    maxServerErrors = 10,
    key = OUTBOX_KEY,
  }) {
    this.storage = storage;
    this.send = send;
    this.sleep = sleep;
    this.now = now;
    this.log = log;
    this.onChange = onChange;
    this.maxItems = maxItems;
    this.attempts = attempts;
    this.baseDelayMs = baseDelayMs;
    this.maxDelayMs = maxDelayMs;
    this.maxServerErrors = maxServerErrors;
    this.key = key;
    this.items = [];
    this._flushing = null;
  }

  get size() {
    return this.items.length;
  }

  async load() {
    const got = await this.storage.get(this.key);
    const arr = got && Array.isArray(got[this.key]) ? got[this.key] : [];
    this.items = arr.filter((it) => it && typeof it.key === "string" && it.payload && typeof it.payload === "object");
    this._notify();
    return this;
  }

  has(key) {
    return this.items.some((it) => it.key === key);
  }

  async _persist() {
    await this.storage.set({ [this.key]: this.items });
    this._notify();
  }

  _notify() {
    try {
      this.onChange(this.items.length);
    } catch {
      /* ignore listener errors */
    }
  }

  /** Queue a payload. Returns false if an item with the same key is already queued. */
  async enqueue(key, payload) {
    if (this.has(key)) return false;
    this.items.push({ key, payload, enqueuedAt: this.now(), attempts: 0, serverErrors: 0, lastError: null });
    while (this.items.length > this.maxItems) {
      const dropped = this.items.shift();
      this.log("warn", `outbox full, dropping oldest item ${dropped.key}`);
    }
    await this._persist();
    return true;
  }

  /** Deliver queued items. Concurrent calls share the same in-flight flush. */
  flush() {
    if (!this._flushing) {
      this._flushing = this._flush().finally(() => {
        this._flushing = null;
      });
    }
    return this._flushing;
  }

  async _flush() {
    const result = { delivered: 0, duplicates: 0, dropped: 0, remaining: 0, stopped: null, lastError: null };
    while (this.items.length) {
      const item = this.items[0];
      try {
        const res = await withRetry(
          async () => {
            item.attempts++;
            try {
              return await this.send(item.payload);
            } catch (err) {
              if (err && err.status >= 500) item.serverErrors = (item.serverErrors || 0) + 1;
              throw err;
            }
          },
          {
            attempts: this.attempts,
            baseMs: this.baseDelayMs,
            maxMs: this.maxDelayMs,
            sleep: this.sleep,
            shouldRetry: (err) => classifyDeliveryError(err) === "retry" && (item.serverErrors || 0) < this.maxServerErrors,
            onRetry: (err, attempt, delay) => this.log("info", `delivery of ${item.key} failed (attempt ${attempt}): ${err.message}; retrying in ${delay} ms`),
          },
        );
        if (this.items[0] === item) this.items.shift();
        if (res && res.data && res.data.status === "duplicate") result.duplicates++;
        else result.delivered++;
        await this._persist();
      } catch (err) {
        result.lastError = err;
        item.lastError = err && err.message ? err.message : String(err);
        const kind = classifyDeliveryError(err);
        if (kind === "drop" || (item.serverErrors || 0) >= this.maxServerErrors) {
          this.log("error", `dropping ${item.key}: ${item.lastError}`);
          if (this.items[0] === item) this.items.shift();
          result.dropped++;
          await this._persist();
          continue;
        }
        result.stopped = kind === "auth" ? "auth" : "unavailable";
        await this._persist();
        break;
      }
    }
    result.remaining = this.items.length;
    return result;
  }
}
