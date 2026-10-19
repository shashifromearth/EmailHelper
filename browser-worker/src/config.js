// Worker configuration, read once from the environment.
// See documents/API.md section 2 for the contract variables.
import path from 'node:path';

export const VERSION = '1.0.0';

export class ConfigError extends Error {}

const TRUE_RE = /^(1|true|yes|on)$/i;
const FALSE_RE = /^(0|false|no|off)$/i;

function intEnv(env, name, def, { min = -Infinity, max = Infinity } = {}) {
  const raw = env[name];
  if (raw === undefined || raw === '') return def;
  const n = Number.parseInt(raw, 10);
  if (!Number.isFinite(n) || n < min || n > max) {
    throw new ConfigError(`${name} must be an integer between ${min} and ${max} (got "${raw}")`);
  }
  return n;
}

export function isLoopbackHost(host) {
  const h = String(host).trim().toLowerCase().replace(/^\[|\]$/g, '');
  return h === 'localhost' || h === '::1' || /^127(\.\d{1,3}){3}$/.test(h);
}

/**
 * Build the config object from an env map. Pure apart from path resolution
 * against `cwd`, so tests can construct configs without touching process.env.
 */
export function loadConfig(env = process.env, cwd = process.cwd()) {
  const host = (env.BROWSER_WORKER_HOST || '127.0.0.1').trim();
  const allowRemote = TRUE_RE.test(env.BROWSER_ALLOW_REMOTE || '');
  if (!isLoopbackHost(host) && !allowRemote) {
    throw new ConfigError(
      `Refusing to bind BROWSER_WORKER_HOST=${host}: only loopback addresses are allowed ` +
        'unless BROWSER_ALLOW_REMOTE=true (set only on an isolated internal network, e.g. docker compose).',
    );
  }

  const token = env.BROWSER_WORKER_TOKEN || '';
  if (!token.trim()) {
    throw new ConfigError('BROWSER_WORKER_TOKEN is required');
  }

  const headlessRaw = env.BROWSER_HEADLESS;
  const headless = headlessRaw === undefined || headlessRaw === '' ? true : !FALSE_RE.test(headlessRaw);

  const screenshotDirRaw = env.BROWSER_SCREENSHOT_DIR || './data/screenshots';
  const uploadDirsRaw = env.BROWSER_ALLOWED_UPLOAD_DIRS || './knowledge;./data';

  return {
    version: VERSION,
    host,
    port: intEnv(env, 'BROWSER_WORKER_PORT', 7801, { min: 0, max: 65535 }),
    allowRemote,
    token,
    profileDir: path.resolve(cwd, env.BROWSER_PROFILE_DIR || './data/browser-profile'),
    headless,
    channel: env.BROWSER_CHANNEL || undefined,
    screenshotDirRaw,
    screenshotDir: path.resolve(cwd, screenshotDirRaw),
    navTimeoutMs: intEnv(env, 'BROWSER_NAV_TIMEOUT_MS', 30000, { min: 1000, max: 300000 }),
    allowedUploadDirs: uploadDirsRaw
      .split(';')
      .map((s) => s.trim())
      .filter(Boolean)
      .map((d) => path.resolve(cwd, d)),
    maxPages: intEnv(env, 'BROWSER_MAX_PAGES', 3, { min: 1, max: 20 }),
    idleTimeoutMs: intEnv(env, 'BROWSER_IDLE_TIMEOUT_MS', 10 * 60 * 1000, { min: 1000 }),
    networkIdleCapMs: intEnv(env, 'BROWSER_NETWORK_IDLE_CAP_MS', 3000, { min: 0, max: 30000 }),
    viewport: { width: 1366, height: 900 },
    bodyLimitBytes: 1024 * 1024,
    linkedinBaseUrl: (env.LINKEDIN_BASE_URL || 'https://www.linkedin.com').replace(/\/+$/, ''),
    // Hard floor of 20 s between LinkedIn navigations; the env var can only make it slower.
    linkedinMinIntervalMs: Math.max(20000, intEnv(env, 'LINKEDIN_MIN_INTERVAL_MS', 20000, { min: 0 })),
    linkedinMaxSearches: intEnv(env, 'LINKEDIN_MAX_SEARCHES_PER_REQUEST', 3, { min: 1, max: 10 }),
    logLevel: (env.BROWSER_LOG_LEVEL || 'info').toLowerCase(),
  };
}
