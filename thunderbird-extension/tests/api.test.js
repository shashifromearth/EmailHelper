import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { createApiClient, ApiError, TOKEN_HEADER, isRetryableError } from "../src/api.js";
import { fakeFetch } from "./_fakes.js";

const cfg = { backendUrl: "http://127.0.0.1:7800/", apiToken: "tok" };

describe("api client", () => {
  test("health is unauthenticated, status carries the token", async () => {
    const fetchFn = fakeFetch((url) => ({ status: 200, body: url.endsWith("/health") ? { status: "ok" } : { version: "1.0.0" } }));
    const api = createApiClient({ getConfig: () => cfg, fetchFn });
    assert.deepEqual(await api.health(), { status: "ok" });
    assert.deepEqual(await api.status(), { version: "1.0.0" });
    assert.equal(fetchFn.calls[0].url, "http://127.0.0.1:7800/health");
    assert.equal(fetchFn.calls[0].init.headers[TOKEN_HEADER], undefined);
    assert.equal(fetchFn.calls[1].url, "http://127.0.0.1:7800/api/v1/status");
    assert.equal(fetchFn.calls[1].init.headers[TOKEN_HEADER], "tok");
  });

  test("missing token fails fast without a request", async () => {
    const fetchFn = fakeFetch(() => ({ status: 200, body: {} }));
    const api = createApiClient({ getConfig: () => ({ ...cfg, apiToken: "" }), fetchFn });
    await assert.rejects(api.status(), (e) => e instanceof ApiError && e.code === "not_configured");
    assert.equal(fetchFn.calls.length, 0);
  });

  test("error body shape is surfaced", async () => {
    const fetchFn = fakeFetch(() => ({ status: 422, body: { error: "validation_failed", message: "subject exceeds 998 characters" } }));
    const api = createApiClient({ getConfig: () => cfg, fetchFn });
    await assert.rejects(api.submitEmail({}), (e) => e.status === 422 && e.code === "validation_failed" && /subject exceeds/.test(e.message) && !isRetryableError(e));
  });

  test("204 ack returns null data; draft id is URL-encoded", async () => {
    const fetchFn = fakeFetch(() => ({ status: 204 }));
    const api = createApiClient({ getConfig: () => cfg, fetchFn });
    const r = await api.ackDraft("d/7 c2", { status: "saved", tb_draft_message_id: 1, error: null });
    assert.equal(r.data, null);
    assert.equal(fetchFn.calls[0].url, "http://127.0.0.1:7800/api/v1/drafts/d%2F7%20c2/ack");
    assert.deepEqual(fetchFn.calls[0].body, { status: "saved", tb_draft_message_id: 1, error: null });
  });

  test("long poll clamps wait and sets a longer timeout", async () => {
    const fetchFn = fakeFetch(() => ({ status: 200, body: { drafts: [] } }));
    const api = createApiClient({ getConfig: () => cfg, fetchFn });
    assert.deepEqual(await api.pendingDrafts(99), { drafts: [] });
    assert.equal(fetchFn.calls[0].url, "http://127.0.0.1:7800/api/v1/drafts/pending?wait=55");
  });

  test("timeout via AbortController", async () => {
    const fetchFn = (url, init) =>
      new Promise((resolve, reject) => {
        init.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      });
    const api = createApiClient({ getConfig: () => cfg, fetchFn, defaultTimeoutMs: 20 });
    await assert.rejects(api.submitEmail({}), (e) => e.timeout === true && isRetryableError(e));
  });

  test("external abort is reported as aborted, not retryable", async () => {
    const fetchFn = (url, init) =>
      new Promise((resolve, reject) => {
        init.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      });
    const api = createApiClient({ getConfig: () => cfg, fetchFn });
    // aborted before the request is sent
    const ac = new AbortController();
    const p = api.pendingDrafts(25, { signal: ac.signal });
    ac.abort();
    await assert.rejects(p, (e) => e.aborted === true && !isRetryableError(e));

    // aborted while the long poll is in flight
    let started;
    const inFlight = new Promise((r) => (started = r));
    const api2 = createApiClient({
      getConfig: () => cfg,
      fetchFn: (url, init) => {
        started();
        return fetchFn(url, init);
      },
    });
    const ac2 = new AbortController();
    const p2 = api2.pendingDrafts(25, { signal: ac2.signal });
    await inFlight;
    ac2.abort();
    await assert.rejects(p2, (e) => e.aborted === true && !isRetryableError(e));
  });
});
