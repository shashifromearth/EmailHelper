// Entry point: node src/index.js
import { loadConfig, ConfigError } from './config.js';
import { createLogger } from './log.js';
import { createApp } from './server.js';

let cfg;
try {
  cfg = loadConfig();
} catch (e) {
  const log = createLogger();
  log.error('invalid configuration', { error: e.message });
  process.exit(e instanceof ConfigError ? 2 : 1);
}

const log = createLogger({ level: cfg.logLevel });
const app = createApp(cfg, log);

try {
  const addr = await app.listen();
  log.info('browser worker listening', {
    host: addr.address,
    port: addr.port,
    headless: cfg.headless,
    profile_dir: cfg.profileDir,
    screenshot_dir: cfg.screenshotDir,
    allowed_upload_dirs: cfg.allowedUploadDirs,
    version: cfg.version,
  });
} catch (e) {
  log.error('failed to listen', { error: e.message });
  process.exit(1);
}

let stopping = false;
async function shutdown(signal) {
  if (stopping) return;
  stopping = true;
  log.info('shutting down', { signal });
  const force = setTimeout(() => {
    log.warn('forced exit after shutdown timeout');
    process.exit(1);
  }, 15000);
  force.unref();
  try {
    await app.close();
  } catch (e) {
    log.error('error during shutdown', { error: e.message });
  }
  log.info('stopped');
  process.exit(0);
}

process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));
process.on('unhandledRejection', (e) => log.error('unhandled rejection', { error: String(e && e.message ? e.message : e) }));
