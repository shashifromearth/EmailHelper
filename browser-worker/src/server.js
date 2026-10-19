// HTTP API (node:http). Contract: documents/API.md section 2.
import http from 'node:http';
import crypto from 'node:crypto';
import { BrowserManager, BrowserUnavailableError, HeadlessError } from './browser.js';
import { extract } from './extract.js';
import { inspect, fill } from './apply.js';
import { createLinkedInPeople } from './linkedin/search.js';
import { isHttpUrl } from './navigation.js';

export class HttpError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

const bad = (msg) => new HttpError(400, 'validation_failed', msg);

function tokenMatches(given, expected) {
  if (typeof given !== 'string' || !given) return false;
  const a = crypto.createHash('sha256').update(given).digest();
  const b = crypto.createHash('sha256').update(expected).digest();
  return crypto.timingSafeEqual(a, b);
}

function sendJson(res, status, body) {
  const data = JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(data),
    'Cache-Control': 'no-store',
    'X-Content-Type-Options': 'nosniff',
  });
  res.end(data);
}

function readJson(req, limit) {
  return new Promise((resolve, reject) => {
    const declared = Number(req.headers['content-length']);
    if (Number.isFinite(declared) && declared > limit) {
      reject(new HttpError(413, 'payload_too_large', `body exceeds ${limit} bytes`));
      req.resume();
      return;
    }
    const chunks = [];
    let size = 0;
    let failed = false;
    req.on('data', (c) => {
      if (failed) return;
      size += c.length;
      if (size > limit) {
        failed = true;
        reject(new HttpError(413, 'payload_too_large', `body exceeds ${limit} bytes`));
        return;
      }
      chunks.push(c);
    });
    req.on('end', () => {
      if (failed) return;
      const raw = Buffer.concat(chunks).toString('utf8');
      if (!raw.trim()) return resolve({});
      try {
        const v = JSON.parse(raw);
        if (!v || typeof v !== 'object' || Array.isArray(v)) return reject(bad('body must be a JSON object'));
        resolve(v);
      } catch {
        reject(bad('body is not valid JSON'));
      }
    });
    req.on('error', reject);
  });
}

// ---- validation -----------------------------------------------------------

function reqUrl(body, key = 'url') {
  const u = body[key];
  if (typeof u !== 'string' || !u.trim()) throw bad(`${key} is required`);
  if (u.length > 4096) throw bad(`${key} is too long`);
  if (!isHttpUrl(u.trim())) throw bad(`${key} must be an absolute http(s) URL`);
  return u.trim();
}

function reqSessionKey(body) {
  const k = body.session_key;
  if (typeof k !== 'string' || !k.trim()) throw bad('session_key is required');
  if (k.length > 200) throw bad('session_key is too long (max 200)');
  return k;
}

function optObject(body, key) {
  const v = body[key];
  if (v === undefined || v === null) return {};
  if (typeof v !== 'object' || Array.isArray(v)) throw bad(`${key} must be an object`);
  return v;
}

// ---- app --------------------------------------------------------------------

export function createApp(cfg, log, { browser } = {}) {
  const mgr = browser || new BrowserManager(cfg, log);
  const linkedinPeople = createLinkedInPeople(mgr, log);

  const routes = {
    'GET /health': async () => ({
      status: 'ok',
      browser: 'chromium',
      headless: cfg.headless,
      version: cfg.version,
      ...mgr.stats(),
    }),
    'POST /v1/extract': async (body) => extract(mgr, { url: reqUrl(body) }),
    'POST /v1/apply/inspect': async (body) => inspect(mgr, { session_key: reqSessionKey(body), url: reqUrl(body) }, log),
    'POST /v1/apply/fill': async (body) => {
      const r = {
        session_key: reqSessionKey(body),
        url: reqUrl(body),
        answers: optObject(body, 'answers'),
        files: optObject(body, 'files'),
        submit: body.submit === true,
      };
      if (body.submit !== undefined && typeof body.submit !== 'boolean') throw bad('submit must be a boolean');
      for (const [k, v] of Object.entries(r.files)) {
        if (typeof v !== 'string') throw bad(`files.${k} must be a string path`);
      }
      return fill(mgr, r, log);
    },
    'POST /v1/linkedin/people': async (body) => {
      const company = body.company;
      if (typeof company !== 'string' || !company.trim()) throw bad('company is required');
      if (company.length > 200) throw bad('company is too long');
      let roles = body.roles ?? ['recruiter'];
      if (!Array.isArray(roles) || roles.some((r) => typeof r !== 'string')) throw bad('roles must be an array of strings');
      roles = roles.map((r) => r.trim()).filter(Boolean);
      let limit = body.limit ?? 10;
      if (!Number.isInteger(limit) || limit < 1) throw bad('limit must be a positive integer');
      limit = Math.min(limit, 50);
      return linkedinPeople({ company: company.trim(), roles, limit });
    },
    'POST /v1/session/login': async (body) => {
      const url = reqUrl(body);
      const finalUrl = await mgr.openLoginPage(url);
      return {
        status: 'opened',
        url: finalUrl,
        message:
          'A browser window is open. Log in manually (including any 2FA), then close the tab. ' +
          'Cookies are stored in the persistent profile and reused by later requests.',
      };
    },
  };
  const knownPaths = new Set(Object.keys(routes).map((k) => k.split(' ')[1]));

  async function handle(req, res) {
    const started = Date.now();
    const url = new URL(req.url, 'http://worker.local');
    const key = `${req.method} ${url.pathname}`;
    const reqId = crypto.randomBytes(6).toString('hex');
    let status = 500;
    try {
      const route = routes[key];
      if (!route) {
        throw knownPaths.has(url.pathname)
          ? new HttpError(405, 'method_not_allowed', `${req.method} not allowed on ${url.pathname}`)
          : new HttpError(404, 'not_found', `no route ${req.method} ${url.pathname}`);
      }
      if (url.pathname !== '/health' && !tokenMatches(req.headers['x-worker-token'], cfg.token)) {
        throw new HttpError(401, 'unauthorized', 'missing or invalid X-Worker-Token');
      }
      const body = req.method === 'POST' ? await readJson(req, cfg.bodyLimitBytes) : {};
      const out = await route(body);
      status = 200;
      sendJson(res, 200, out);
    } catch (e) {
      if (e instanceof HttpError) status = e.status;
      else if (e instanceof HeadlessError) status = 409;
      else if (e instanceof BrowserUnavailableError) status = 503;
      else status = 500;
      const code =
        e instanceof HttpError ? e.code : e instanceof HeadlessError ? 'headless_mode' : e instanceof BrowserUnavailableError ? 'browser_unavailable' : 'internal_error';
      if (status >= 500) log.error('request failed', { req_id: reqId, route: key, error: e.message.split('\n')[0] });
      if (!res.headersSent) sendJson(res, status, { error: code, message: e.message.split('\n')[0].slice(0, 500) });
      else res.destroy();
    } finally {
      log.info('request', { req_id: reqId, method: req.method, path: url.pathname, status, ms: Date.now() - started });
    }
  }

  const server = http.createServer((req, res) => {
    handle(req, res).catch(() => res.destroy());
  });
  server.requestTimeout = 0; // fill/linkedin can take a while; the orchestrator owns the timeout
  server.headersTimeout = 30000;

  return {
    server,
    browser: mgr,
    listen: () =>
      new Promise((resolve, reject) => {
        server.once('error', reject);
        server.listen(cfg.port, cfg.host, () => {
          server.off('error', reject);
          resolve(server.address());
        });
      }),
    close: async ({ graceMs = 5000 } = {}) => {
      const closed = new Promise((r) => server.close(() => r()));
      server.closeIdleConnections?.();
      await Promise.race([closed, new Promise((r) => setTimeout(r, graceMs).unref())]);
      server.closeAllConnections?.();
      await mgr.close();
    },
  };
}
