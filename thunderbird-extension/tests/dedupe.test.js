import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { LruSet, DedupeStore, dedupeKey, SEEN_KEY } from "../src/dedupe.js";
import { MemoryStorage } from "./_fakes.js";

describe("LruSet", () => {
  test("evicts least recently added beyond capacity", () => {
    const s = new LruSet(3);
    s.add("a").add("b").add("c");
    s.add("a"); // refresh a
    s.add("d"); // evicts b
    assert.deepEqual(s.toArray(), ["c", "a", "d"]);
    assert.equal(s.has("b"), false);
    assert.equal(s.size, 3);
  });

  test("caps at 5000 by default", () => {
    const s = new LruSet();
    for (let i = 0; i < 5100; i++) s.add(`k${i}`);
    assert.equal(s.size, 5000);
    assert.equal(s.has("k99"), false);
    assert.equal(s.has("k100"), true);
    assert.equal(s.has("k5099"), true);
  });
});

describe("DedupeStore", () => {
  test("key format", () => {
    assert.equal(dedupeKey("account3", " <a@b> "), "account3|<a@b>");
  });

  test("persists and reloads (survives restart)", async () => {
    const storage = new MemoryStorage();
    const d1 = await new DedupeStore({ storage, debounceMs: 0 }).load();
    await d1.add("account1|<x@y>");
    await d1.add("account2|<x@y>");
    assert.deepEqual(storage.data[SEEN_KEY], ["account1|<x@y>", "account2|<x@y>"]);

    const d2 = await new DedupeStore({ storage }).load();
    assert.equal(d2.has("account1|<x@y>"), true);
    assert.equal(d2.has("account3|<x@y>"), false);
  });

  test("debounced writes are coalesced and flushable", async () => {
    const storage = new MemoryStorage();
    const d = await new DedupeStore({ storage, debounceMs: 10000, capacity: 2 }).load();
    await d.add("a");
    await d.add("b");
    await d.add("c");
    assert.equal(storage.writes, 0);
    await d.flush();
    assert.equal(storage.writes, 1);
    assert.deepEqual(storage.data[SEEN_KEY], ["b", "c"]);
    await d.flush(); // nothing dirty
    assert.equal(storage.writes, 1);
  });

  test("ignores corrupt stored data", async () => {
    const storage = new MemoryStorage({ [SEEN_KEY]: ["ok", 5, null] });
    const d = await new DedupeStore({ storage }).load();
    assert.equal(d.size, 1);
  });
});
