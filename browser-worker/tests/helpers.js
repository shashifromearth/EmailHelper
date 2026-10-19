// Shared test helpers: mock-site server + in-process worker on random ports.
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadConfig } from '../src/config.js';
import { createLogger } from '../src/log.js';
import { createApp } from '../src/server.js';
import { startMockServer } from '../../tests/mock-sites/server.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(HERE, '..', '..');
export const MOCK_DIR = path.join(REPO_ROOT, 'tests', 'mock-sites');
export const FIXTURES_DIR = path.join(MOCK_DIR, 'fixtures');
export const RESUME = path.join(FIXTURES_DIR, 'resume.pdf');
export const TOKEN = 'test-token-0123456789abcdef';

export async function startMock() {
  return startMockServer({ port: 0 });
}

export async function makeTmpDir(prefix = 'eh-bw-') {
  return fs.mkdtemp(path.join(os.tmpdir(), prefix));
}

export async function rmrf(dir) {
  await fs.rm(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }).catch(() => {});
}

export function testEnv(tmp, extra = {}) {
  return {
    BROWSER_WORKER_TOKEN: TOKEN,
    BROWSER_WORKER_PORT: '0',
    BROWSER_PROFILE_DIR: path.join(tmp, 'profile'),
    BROWSER_SCREENSHOT_DIR: path.join(tmp, 'screenshots'),
    BROWSER_ALLOWED_UPLOAD_DIRS: FIXTURES_DIR,
    BROWSER_HEADLESS: 'true',
    BROWSER_NETWORK_IDLE_CAP_MS: '1000',
    BROWSER_NAV_TIMEOUT_MS: '15000',
    BROWSER_LOG_LEVEL: process.env.TEST_LOG_LEVEL || 'silent',
    ...extra,
  };
}

export async function startWorker(extraEnv = {}) {
  const tmp = await makeTmpDir();
  const cfg = loadConfig(testEnv(tmp, extraEnv), tmp);
  const app = createApp(cfg, createLogger({ level: cfg.logLevel }));
  const addr = await app.listen();
  const base = `http://127.0.0.1:${addr.port}`;
  const call = async (method, p, body, { token = TOKEN, raw = false } = {}) => {
    const headers = { 'Content-Type': 'application/json' };
    if (token) headers['X-Worker-Token'] = token;
    const res = await fetch(base + p, {
      method,
      headers,
      body: body === undefined ? undefined : raw ? body : JSON.stringify(body),
    });
    const text = await res.text();
    let json = null;
    try {
      json = JSON.parse(text);
    } catch {}
    return { status: res.status, body: json, text };
  };
  return {
    cfg,
    app,
    base,
    tmp,
    call,
    close: async () => {
      await app.close({ graceMs: 1000 });
      await rmrf(tmp);
    },
  };
}
