import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { inflateRawSync } from "node:zlib";
import { readFileSync, existsSync, mkdtempSync, rmSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createZip, crc32, collectFiles, build } from "../scripts/build.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/** Minimal ZIP reader for verification (central directory based). */
function readZip(buf) {
  const eocd = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert.ok(eocd >= 0, "EOCD present");
  const count = buf.readUInt16LE(eocd + 10);
  let p = buf.readUInt32LE(eocd + 16);
  const out = new Map();
  for (let i = 0; i < count; i++) {
    assert.equal(buf.readUInt32LE(p), 0x02014b50);
    const method = buf.readUInt16LE(p + 10);
    const crc = buf.readUInt32LE(p + 16);
    const csize = buf.readUInt32LE(p + 20);
    const usize = buf.readUInt32LE(p + 24);
    const nlen = buf.readUInt16LE(p + 28);
    const elen = buf.readUInt16LE(p + 30);
    const clen = buf.readUInt16LE(p + 32);
    const off = buf.readUInt32LE(p + 42);
    const name = buf.toString("utf8", p + 46, p + 46 + nlen);
    assert.equal(buf.readUInt32LE(off), 0x04034b50);
    const lnlen = buf.readUInt16LE(off + 26);
    const lelen = buf.readUInt16LE(off + 28);
    const raw = buf.subarray(off + 30 + lnlen + lelen, off + 30 + lnlen + lelen + csize);
    const data = method === 8 ? inflateRawSync(raw) : Buffer.from(raw);
    assert.equal(data.length, usize);
    assert.equal(crc32(data), crc, `crc of ${name}`);
    out.set(name, data);
    p += 46 + nlen + elen + clen;
  }
  return out;
}

describe("build", () => {
  test("crc32 matches the reference value", () => {
    assert.equal(crc32(Buffer.from("123456789")), 0xcbf43926);
    assert.equal(crc32(Buffer.alloc(0)), 0);
  });

  test("zip round trip (stored and deflated entries)", () => {
    const entries = [
      { name: "a.txt", data: Buffer.from("x") },
      { name: "dir/b.json", data: Buffer.from(JSON.stringify({ k: "v".repeat(1000) })) },
      { name: "empty", data: Buffer.alloc(0) },
    ];
    const files = readZip(createZip(entries));
    assert.deepEqual([...files.keys()], ["a.txt", "dir/b.json", "empty"]);
    for (const e of entries) assert.deepEqual(files.get(e.name), e.data);
  });

  test("xpi contains the manifest at the root and no tests/scripts", () => {
    const dir = mkdtempSync(join(tmpdir(), "eh-xpi-"));
    try {
      const out = join(dir, "x.xpi");
      const r = build({ out });
      const files = readZip(readFileSync(out));
      assert.equal(files.size, r.files);
      assert.ok(files.has("manifest.json"));
      assert.ok(files.has("background.js"));
      assert.ok(files.has("src/drafts.js"));
      assert.ok(![...files.keys()].some((n) => n.startsWith("tests/") || n.startsWith("scripts/") || n === "package.json"));
      assert.deepEqual(files.get("manifest.json"), readFileSync(join(root, "manifest.json")));
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("manifest", () => {
  const manifest = JSON.parse(readFileSync(join(root, "manifest.json"), "utf8"));
  const packaged = new Set(collectFiles().map((f) => f.name));

  test("MV2, id and minimum version", () => {
    assert.equal(manifest.manifest_version, 2);
    assert.equal(manifest.browser_specific_settings.gecko.id, "emailhelper@local");
    assert.equal(manifest.browser_specific_settings.gecko.strict_min_version, "115.0");
  });

  test("permissions are exactly what the extension needs", () => {
    assert.deepEqual([...manifest.permissions].sort(), [
      "accountsRead",
      "compose",
      "compose.save",
      "http://127.0.0.1/*",
      "http://localhost/*",
      "messagesRead",
      "notifications",
      "storage",
    ]);
  });

  test("every file referenced by the manifest and HTML pages is packaged", () => {
    const refs = [manifest.background.page, manifest.browser_action.default_popup, manifest.browser_action.default_icon, manifest.options_ui.page, ...Object.values(manifest.icons)];
    for (const r of refs) assert.ok(packaged.has(r), `${r} packaged`);
    for (const page of [manifest.background.page, manifest.browser_action.default_popup, manifest.options_ui.page]) {
      const html = readFileSync(join(root, page), "utf8");
      for (const m of html.matchAll(/(?:src|href)="([^"#:]+)"/g)) {
        const resolved = join(dirname(page), m[1]).split("\\").join("/");
        assert.ok(packaged.has(resolved), `${page} -> ${resolved}`);
        assert.ok(existsSync(join(root, resolved)));
      }
    }
  });

  test("module imports resolve to packaged files", () => {
    for (const f of packaged) {
      if (!f.endsWith(".js")) continue;
      const src = readFileSync(join(root, f), "utf8");
      for (const m of src.matchAll(/^import[^"']*["']([^"']+)["']/gm)) {
        const resolved = join(dirname(f), m[1]).split("\\").join("/");
        assert.ok(packaged.has(resolved), `${f} imports ${m[1]}`);
      }
    }
  });
});
