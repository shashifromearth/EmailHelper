// Persistent Chromium context lifecycle, page pool and concurrency control.
import fs from 'node:fs/promises';
import { chromium } from 'playwright';
import { Semaphore, KeyedMutex } from './sync.js';
import { trackMainStatus } from './blockers.js';

export class BrowserUnavailableError extends Error {}
export class HeadlessError extends Error {}

export class BrowserManager {
  constructor(cfg, log, { chromiumImpl = chromium } = {}) {
    this.cfg = cfg;
    this.log = log;
    this.chromium = chromiumImpl;
    this.context = null;
    this.launching = null;
    this.sessions = new Map(); // session_key -> { key, page, lastUsed, busy, url }
    this.ephemeral = new Set();
    this.loginPage = null;
    this.sem = new Semaphore(cfg.maxPages);
    this.locks = new KeyedMutex();
    this.closing = false;
    this.reaper = setInterval(() => this.reapIdle().catch(() => {}), Math.min(30000, cfg.idleTimeoutMs));
    this.reaper.unref();
  }

  get running() {
    return !!this.context;
  }

  stats() {
    return {
      browser_running: this.running,
      open_sessions: this.sessions.size,
      active_operations: this.sem.active,
      queued_operations: this.sem.pending,
    };
  }

  async getContext() {
    if (this.closing) throw new BrowserUnavailableError('worker is shutting down');
    if (this.context) return this.context;
    if (!this.launching) {
      this.launching = this._launch().finally(() => {
        this.launching = null;
      });
    }
    return this.launching;
  }

  async _launch() {
    const { profileDir, headless, viewport, channel, navTimeoutMs } = this.cfg;
    await fs.mkdir(profileDir, { recursive: true });
    let ctx;
    try {
      ctx = await this.chromium.launchPersistentContext(profileDir, {
        headless,
        viewport,
        acceptDownloads: false,
        ...(channel ? { channel } : {}),
      });
    } catch (e) {
      this.log.error('browser launch failed', { error: e.message.split('\n')[0] });
      throw new BrowserUnavailableError(`failed to launch Chromium: ${e.message.split('\n')[0]}`);
    }
    ctx.setDefaultNavigationTimeout(navTimeoutMs);
    ctx.setDefaultTimeout(10000);
    ctx.on('close', () => {
      if (this.context !== ctx) return;
      this.context = null;
      this.sessions.clear();
      this.ephemeral.clear();
      this.loginPage = null;
      if (!this.closing) this.log.warn('browser context closed unexpectedly; it will be relaunched on the next request');
    });
    this.context = ctx;
    this.log.info('browser launched', { headless, profile_dir: profileDir });
    return ctx;
  }

  _openCount() {
    let n = this.ephemeral.size;
    for (const s of this.sessions.values()) if (s.page && !s.page.isClosed()) n++;
    return n;
  }

  /** Close least-recently-used idle session pages so at most maxPages are open. */
  async _makeRoom() {
    while (this._openCount() >= this.cfg.maxPages) {
      const idle = [...this.sessions.values()].filter((s) => !s.busy && s.page).sort((a, b) => a.lastUsed - b.lastUsed)[0];
      if (!idle) break;
      await this.closeSession(idle.key, 'evicted');
    }
  }

  async _newPage() {
    const ctx = await this.getContext();
    await this._makeRoom();
    const page = await ctx.newPage();
    trackMainStatus(page);
    page.on('crash', () => {
      this.log.warn('page crashed', { url: page.url() });
      page.close().catch(() => {});
    });
    return page;
  }

  _attach(entry, page) {
    entry.page = page;
    page.on('close', () => {
      if (this.sessions.get(entry.key) === entry && entry.page === page) this.sessions.delete(entry.key);
    });
  }

  /** Replace the page of a session (e.g. an Apply button opened a popup). */
  adoptPage(entry, page) {
    const old = entry.page;
    trackMainStatus(page);
    this._attach(entry, page);
    if (old && old !== page) old.close().catch(() => {});
  }

  /**
   * Run fn(entry, { fresh }) with the session's page, serialised per session_key
   * and limited to cfg.maxPages concurrent operations overall.
   */
  async withSession(key, fn) {
    return this.locks.run(`s:${key}`, async () => {
      await this.sem.acquire();
      try {
        let entry = this.sessions.get(key);
        if (entry && (!entry.page || entry.page.isClosed())) {
          this.sessions.delete(key);
          entry = null;
        }
        let fresh = false;
        if (!entry) {
          entry = { key, page: null, lastUsed: Date.now(), busy: true, url: null };
          this.sessions.set(key, entry);
          try {
            this._attach(entry, await this._newPage());
          } catch (e) {
            this.sessions.delete(key);
            throw e;
          }
          fresh = true;
        }
        entry.busy = true;
        try {
          return await fn(entry, { fresh });
        } finally {
          entry.busy = false;
          entry.lastUsed = Date.now();
        }
      } finally {
        this.sem.release();
      }
    });
  }

  /** Run fn(page) on a throwaway page that is closed afterwards. */
  async withEphemeralPage(fn) {
    await this.sem.acquire();
    let page = null;
    try {
      page = await this._newPage();
      this.ephemeral.add(page);
      return await fn(page);
    } finally {
      if (page) {
        this.ephemeral.delete(page);
        await page.close().catch(() => {});
      }
      this.sem.release();
    }
  }

  async closeSession(key, reason = 'done') {
    const entry = this.sessions.get(key);
    if (!entry) return;
    this.sessions.delete(key);
    this.log.debug('closing session page', { session_key: key, reason });
    if (entry.page) await entry.page.close().catch(() => {});
  }

  async reapIdle() {
    const now = Date.now();
    for (const entry of [...this.sessions.values()]) {
      if (!entry.busy && now - entry.lastUsed > this.cfg.idleTimeoutMs) await this.closeSession(entry.key, 'idle');
    }
  }

  /** Headed-only: open a window for the human to log in manually. */
  async openLoginPage(url) {
    if (this.cfg.headless) {
      throw new HeadlessError(
        'The worker runs headless, so there is no window to log in with. Restart it with BROWSER_HEADLESS=false ' +
          '(on a machine with a display), call /v1/session/login again, log in, then restart headless.',
      );
    }
    const ctx = await this.getContext();
    if (!this.loginPage || this.loginPage.isClosed()) {
      this.loginPage = await ctx.newPage();
      trackMainStatus(this.loginPage);
    }
    await this.loginPage.goto(url, { waitUntil: 'domcontentloaded' });
    await this.loginPage.bringToFront().catch(() => {});
    return this.loginPage.url();
  }

  async close() {
    this.closing = true;
    clearInterval(this.reaper);
    if (this.launching) await this.launching.catch(() => {});
    const ctx = this.context;
    if (ctx) {
      await ctx.close().catch(() => {});
      this.context = null;
    }
    this.sessions.clear();
  }
}
