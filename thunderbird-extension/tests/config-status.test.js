import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { validateBackendUrl, normalizeConfig, isAccountEnabled, loadConfig, saveConfig, CONFIG_KEY, DEFAULT_BACKEND_URL } from "../src/config.js";
import { computeBadge, StateStore, BADGE_COLORS, STATE_KEY } from "../src/status.js";
import { MemoryStorage } from "./_fakes.js";

describe("config", () => {
  test("backend URL must be loopback http", () => {
    assert.deepEqual(validateBackendUrl("http://127.0.0.1:7800/"), { ok: true, url: "http://127.0.0.1:7800" });
    assert.deepEqual(validateBackendUrl(" http://localhost:9000/x "), { ok: true, url: "http://localhost:9000" });
    assert.equal(validateBackendUrl("https://127.0.0.1:7800").ok, false);
    assert.equal(validateBackendUrl("http://example.com").ok, false);
    assert.equal(validateBackendUrl("http://u:p@127.0.0.1").ok, false);
    assert.equal(validateBackendUrl("nonsense").ok, false);
    assert.equal(validateBackendUrl("").ok, false);
  });

  test("defaults and normalisation", () => {
    const c = normalizeConfig(undefined);
    assert.equal(c.backendUrl, DEFAULT_BACKEND_URL);
    assert.equal(c.apiToken, "");
    assert.equal(c.notifyOnDraft, true);
    const c2 = normalizeConfig({ backendUrl: "http://evil.com", apiToken: "  t  ", notifyOnDraft: false, accounts: { a: false, b: "yes" } });
    assert.equal(c2.backendUrl, DEFAULT_BACKEND_URL);
    assert.equal(c2.apiToken, "t");
    assert.equal(c2.notifyOnDraft, false);
    assert.deepEqual(c2.accounts, { a: false, b: true });
  });

  test("accounts are enabled unless explicitly disabled", () => {
    const c = normalizeConfig({ accounts: { account2: false } });
    assert.equal(isAccountEnabled(c, "account1"), true);
    assert.equal(isAccountEnabled(c, "account2"), false);
    assert.equal(isAccountEnabled(c, null), false);
  });

  test("round trip through storage.local", async () => {
    const storage = new MemoryStorage();
    await saveConfig(storage, { backendUrl: "http://localhost:7800", apiToken: "abc" });
    assert.equal(storage.data[CONFIG_KEY].apiToken, "abc");
    const c = await loadConfig(storage);
    assert.equal(c.backendUrl, "http://localhost:7800");
  });
});

describe("badge", () => {
  test("priority: not configured > auth > unreachable > failures > queued > new drafts", () => {
    assert.equal(computeBadge({ notConfigured: true }).text, "?");
    assert.deepEqual(computeBadge({ authError: true, outboxDepth: 3 }).text, "!");
    assert.equal(computeBadge({ backendReachable: false, outboxDepth: 3 }).color, BADGE_COLORS.error);
    assert.equal(computeBadge({ backendReachable: true, unseenFailures: 1, outboxDepth: 2 }).text, "!");
    assert.deepEqual(computeBadge({ backendReachable: true, outboxDepth: 3, unseenDrafts: 2 }).text, "3");
    assert.equal(computeBadge({ backendReachable: true, outboxDepth: 3 }).color, BADGE_COLORS.queued);
    assert.equal(computeBadge({ backendReachable: true, unseenDrafts: 120 }).text, "99+");
    assert.equal(computeBadge({ backendReachable: true }).text, "");
    assert.equal(computeBadge(null).text, "");
  });

  test("StateStore serialises updates and notifies", async () => {
    const storage = new MemoryStorage();
    const seen = [];
    const s = new StateStore({ storage, onChange: (st) => seen.push(st.draftsSaved) });
    await s.load();
    await Promise.all([1, 2, 3].map(() => s.update((st) => ({ draftsSaved: st.draftsSaved + 1 }))));
    assert.equal(s.get().draftsSaved, 3);
    assert.deepEqual(seen, [1, 2, 3]);
    await s.recordError("boom", 42);
    assert.deepEqual(storage.data[STATE_KEY].lastError, { message: "boom", at: 42 });
  });
});
