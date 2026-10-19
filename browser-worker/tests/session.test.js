// Session persistence: cookies survive a context restart with the same profile dir.
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { loadConfig } from '../src/config.js';
import { createLogger } from '../src/log.js';
import { BrowserManager } from '../src/browser.js';
import { startMock, makeTmpDir, rmrf, testEnv } from './helpers.js';

describe('persistent profile', () => {
  let mock;
  let tmp;
  before(async () => {
    mock = await startMock();
    tmp = await makeTmpDir('eh-bw-profile-');
  });
  after(async () => {
    await mock?.close();
    await rmrf(tmp);
  });

  it('keeps cookies across a browser restart', async () => {
    const cfg = loadConfig(testEnv(tmp), tmp);
    assert.equal(cfg.profileDir, path.join(tmp, 'profile'));
    const log = createLogger({ level: 'silent' });

    const m1 = new BrowserManager(cfg, log);
    await m1.withEphemeralPage(async (page) => {
      await page.goto(`${mock.url}/set-cookie?v=persist-me`);
    });
    const before1 = await (await m1.getContext()).cookies(mock.url);
    assert.ok(before1.some((c) => c.name === 'eh_session'));
    await m1.close();

    const m2 = new BrowserManager(cfg, log);
    const cookies = await (await m2.getContext()).cookies(mock.url);
    await m2.close();
    const c = cookies.find((x) => x.name === 'eh_session');
    assert.ok(c, 'cookie should survive the restart');
    assert.equal(c.value, 'persist-me');
  });

  it('relaunches the context after it is closed', async () => {
    const cfg = loadConfig(testEnv(tmp, { BROWSER_PROFILE_DIR: path.join(tmp, 'profile2') }), tmp);
    const m = new BrowserManager(cfg, createLogger({ level: 'silent' }));
    try {
      const ctx1 = await m.getContext();
      await ctx1.close(); // simulate a crash
      assert.equal(m.running, false);
      const title = await m.withEphemeralPage(async (page) => {
        await page.goto(`${mock.url}/greenhouse/job.html`);
        return page.title();
      });
      assert.match(title, /Staff AI Engineer/);
    } finally {
      await m.close();
    }
  });
});
