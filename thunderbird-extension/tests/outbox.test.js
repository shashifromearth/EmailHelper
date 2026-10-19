import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { Outbox, OUTBOX_KEY, classifyDeliveryError } from "../src/outbox.js";
import { createApiClient, ApiError, backoffDelay, withRetry, TOKEN_HEADER } from "../src/api.js";
import { MemoryStorage, fakeFetch, networkDown, recordingSleep } from "./_fakes.js";

const cfg = { backendUrl: "http://127.0.0.1:7800", apiToken: "secret-token" };
const payload = (n) => ({ account_id: "account1", message_id: `<m${n}@x>`, tb_message_id: n, from: "a@b", subject: "s", body_text: "b" });

function makeOutbox(fetchFn, storage = new MemoryStorage(), sleep = recordingSleep()) {
  const api = createApiClient({ getConfig: async () => cfg, fetchFn });
  const outbox = new Outbox({ storage, send: (p) => api.submitEmail(p), sleep });
  return { outbox, storage, sleep };
}

describe("backoff helpers", () => {
  test("exponential, capped", () => {
    assert.deepEqual([1, 2, 3, 4, 5, 6].map((a) => backoffDelay(a, { baseMs: 1000, maxMs: 10000 })), [1000, 2000, 4000, 8000, 10000, 10000]);
  });

  test("withRetry stops at max attempts", async () => {
    const sleep = recordingSleep();
    let n = 0;
    await assert.rejects(
      withRetry(async () => {
        n++;
        throw new ApiError("down", { network: true });
      }, { attempts: 5, sleep }),
    );
    assert.equal(n, 5);
    assert.deepEqual(sleep.delays, [1000, 2000, 4000, 8000]);
  });

  test("withRetry does not retry permanent errors", async () => {
    let n = 0;
    await assert.rejects(withRetry(async () => {
      n++;
      throw new ApiError("bad", { status: 422 });
    }, { sleep: recordingSleep() }));
    assert.equal(n, 1);
  });

  test("classification", () => {
    assert.equal(classifyDeliveryError(new ApiError("x", { network: true })), "retry");
    assert.equal(classifyDeliveryError(new ApiError("x", { timeout: true })), "retry");
    assert.equal(classifyDeliveryError(new ApiError("x", { status: 503 })), "retry");
    assert.equal(classifyDeliveryError(new ApiError("x", { status: 429 })), "retry");
    assert.equal(classifyDeliveryError(new ApiError("x", { status: 401 })), "auth");
    assert.equal(classifyDeliveryError(new ApiError("x", { code: "not_configured" })), "auth");
    assert.equal(classifyDeliveryError(new ApiError("x", { status: 400 })), "drop");
    assert.equal(classifyDeliveryError(new ApiError("x", { status: 413 })), "drop");
  });
});

describe("Outbox", () => {
  test("delivers in order with the token header and empties the queue", async () => {
    const fetchFn = fakeFetch(() => ({ status: 202, body: { email_id: "e", job_id: "j", status: "queued" } }));
    const { outbox, storage } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    await outbox.enqueue("k2", payload(2));
    assert.equal(await outbox.enqueue("k1", payload(1)), false, "duplicate key not queued twice");
    const r = await outbox.flush();
    assert.equal(r.delivered, 2);
    assert.equal(r.remaining, 0);
    assert.deepEqual(storage.data[OUTBOX_KEY], []);
    assert.equal(fetchFn.calls.length, 2);
    assert.equal(fetchFn.calls[0].url, "http://127.0.0.1:7800/api/v1/emails");
    assert.equal(fetchFn.calls[0].init.method, "POST");
    assert.equal(fetchFn.calls[0].init.headers[TOKEN_HEADER], "secret-token");
    assert.equal(fetchFn.calls[0].init.headers["Content-Type"], "application/json");
    assert.deepEqual(fetchFn.calls.map((c) => c.body.tb_message_id), [1, 2]);
  });

  test("200 duplicate counts as delivered-duplicate", async () => {
    const fetchFn = fakeFetch(() => ({ status: 200, body: { email_id: "e", job_id: null, status: "duplicate" } }));
    const { outbox } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    const r = await outbox.flush();
    assert.equal(r.duplicates, 1);
    assert.equal(r.delivered, 0);
    assert.equal(outbox.size, 0);
  });

  test("retries with backoff, max 5 attempts, keeps item when backend stays down", async () => {
    const fetchFn = fakeFetch(networkDown);
    const { outbox, storage, sleep } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    await outbox.enqueue("k2", payload(2));
    const r = await outbox.flush();
    assert.equal(r.stopped, "unavailable");
    assert.equal(r.remaining, 2);
    assert.equal(fetchFn.calls.length, 5, "only the head item is attempted while the backend is down");
    assert.deepEqual(sleep.delays, [1000, 2000, 4000, 8000]);
    assert.equal(storage.data[OUTBOX_KEY].length, 2);
    assert.match(storage.data[OUTBOX_KEY][0].lastError, /unreachable/);
  });

  test("recovers after transient failures", async () => {
    const fetchFn = fakeFetch((url, init, n) => (n <= 2 ? networkDown() : { status: 202, body: { status: "queued" } }));
    const { outbox } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    const r = await outbox.flush();
    assert.equal(r.delivered, 1);
    assert.equal(fetchFn.calls.length, 3);
  });

  test("survives a restart: queue is restored from storage and delivered later", async () => {
    const storage = new MemoryStorage();
    const down = makeOutbox(fakeFetch(networkDown), storage);
    await down.outbox.enqueue("k1", payload(1));
    await down.outbox.flush();

    // "Thunderbird restarts": a brand new Outbox over the same storage.
    const upFetch = fakeFetch(() => ({ status: 202, body: { status: "queued" } }));
    const up = makeOutbox(upFetch, storage);
    await up.outbox.load();
    assert.equal(up.outbox.size, 1);
    const r = await up.outbox.flush();
    assert.equal(r.delivered, 1);
    assert.equal(upFetch.calls[0].body.message_id, "<m1@x>");
    assert.deepEqual(storage.data[OUTBOX_KEY], []);
  });

  test("permanent 4xx is dropped so it cannot block the queue", async () => {
    const fetchFn = fakeFetch((url, init) =>
      JSON.parse(init.body).tb_message_id === 1
        ? { status: 400, body: { error: "validation_failed", message: "subject exceeds 998 characters" } }
        : { status: 202, body: { status: "queued" } },
    );
    const { outbox } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    await outbox.enqueue("k2", payload(2));
    const r = await outbox.flush();
    assert.equal(r.dropped, 1);
    assert.equal(r.delivered, 1);
    assert.equal(r.lastError.code, "validation_failed");
    assert.equal(fetchFn.calls.length, 2);
  });

  test("401 stops the flush without retrying and keeps the items", async () => {
    const fetchFn = fakeFetch(() => ({ status: 401, body: { error: "unauthorized", message: "bad token" } }));
    const { outbox, sleep } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    const r = await outbox.flush();
    assert.equal(r.stopped, "auth");
    assert.equal(outbox.size, 1);
    assert.equal(fetchFn.calls.length, 1);
    assert.deepEqual(sleep.delays, []);
  });

  test("poison item (persistent 5xx) is eventually dropped", async () => {
    const fetchFn = fakeFetch(() => ({ status: 500, body: { error: "internal", message: "boom" } }));
    const storage = new MemoryStorage();
    const api = createApiClient({ getConfig: async () => cfg, fetchFn });
    const outbox = new Outbox({ storage, send: (p) => api.submitEmail(p), sleep: recordingSleep(), maxServerErrors: 3 });
    await outbox.enqueue("k1", payload(1));
    const r = await outbox.flush();
    assert.equal(r.dropped, 1);
    assert.equal(fetchFn.calls.length, 3);
  });

  test("concurrent flush calls share one run", async () => {
    let release;
    const gate = new Promise((r) => (release = r));
    const fetchFn = fakeFetch(async () => {
      await gate;
      return { status: 202, body: { status: "queued" } };
    });
    const { outbox } = makeOutbox(fetchFn);
    await outbox.enqueue("k1", payload(1));
    const a = outbox.flush();
    const b = outbox.flush();
    assert.equal(a, b);
    release();
    await a;
    assert.equal(fetchFn.calls.length, 1);
  });

  test("caps queue length by dropping the oldest", async () => {
    const storage = new MemoryStorage();
    const outbox = new Outbox({ storage, send: async () => ({}), maxItems: 2 });
    await outbox.enqueue("k1", payload(1));
    await outbox.enqueue("k2", payload(2));
    await outbox.enqueue("k3", payload(3));
    assert.deepEqual(outbox.items.map((i) => i.key), ["k2", "k3"]);
  });
});
