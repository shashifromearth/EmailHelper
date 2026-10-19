// Small concurrency primitives: counting semaphore, per-key mutex, interval limiter.

export class Semaphore {
  constructor(max) {
    this.max = max;
    this.active = 0;
    this.queue = [];
  }

  async acquire() {
    if (this.active < this.max) {
      this.active++;
      return;
    }
    await new Promise((resolve) => this.queue.push(resolve));
  }

  release() {
    const next = this.queue.shift();
    if (next) next(); // hand the slot over directly
    else this.active--;
  }

  get pending() {
    return this.queue.length;
  }
}

/** Serialises async work per key (e.g. per session_key). */
export class KeyedMutex {
  constructor() {
    this.tails = new Map();
  }

  async run(key, fn) {
    const prev = this.tails.get(key) || Promise.resolve();
    let release;
    const gate = new Promise((r) => (release = r));
    const tail = prev.then(() => gate);
    this.tails.set(key, tail);
    await prev;
    try {
      return await fn();
    } finally {
      release();
      if (this.tails.get(key) === tail) this.tails.delete(key);
    }
  }
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Guarantees at least `intervalMs` between successive `wait()` resolutions,
 * across all callers (calls are queued in order).
 */
export class IntervalLimiter {
  constructor(intervalMs, { now = Date.now, sleepFn = sleep } = {}) {
    this.intervalMs = intervalMs;
    this.now = now;
    this.sleep = sleepFn;
    this.last = -Infinity;
    this.chain = Promise.resolve();
  }

  wait() {
    const p = this.chain.then(async () => {
      const delay = this.last + this.intervalMs - this.now();
      if (delay > 0) await this.sleep(delay);
      this.last = this.now();
    });
    this.chain = p.catch(() => {});
    return p;
  }
}

/** Poll `fn` until it returns a truthy value or the timeout elapses. Returns the last value. */
export async function pollUntil(fn, { timeoutMs, intervalMs = 250 }) {
  const deadline = Date.now() + timeoutMs;
  let value;
  for (;;) {
    try {
      value = await fn();
    } catch {
      value = undefined;
    }
    if (value) return value;
    if (Date.now() >= deadline) return value;
    await sleep(intervalMs);
  }
}
