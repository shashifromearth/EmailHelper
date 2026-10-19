// Fetch wrapper for the EmailHelper orchestrator (documents/API.md section 1).
// - per-request timeout via AbortController
// - X-EmailHelper-Token on every /api/* call
// - errors normalised to ApiError ({ error, message } body shape)

export const TOKEN_HEADER = "X-EmailHelper-Token";
export const DEFAULT_TIMEOUT_MS = 15000;

export class ApiError extends Error {
  constructor(message, { status = 0, code = null, network = false, timeout = false, aborted = false, body = null } = {}) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.network = network;
    this.timeout = timeout;
    this.aborted = aborted;
    this.body = body;
  }
}

/** Transient failures worth retrying: backend down, timeouts, 5xx, 408/425/429. */
export function isRetryableError(err) {
  if (!err) return false;
  if (err.aborted) return false;
  if (err.network || err.timeout) return true;
  const s = err.status;
  if (!s) return err.code !== "not_configured";
  return s >= 500 || s === 408 || s === 425 || s === 429;
}

export function isAuthError(err) {
  return !!err && (err.status === 401 || err.status === 403 || err.code === "not_configured");
}

/** Exponential backoff: base * factor^(attempt-1), capped, with optional jitter in [0, jitter). */
export function backoffDelay(attempt, { baseMs = 1000, factor = 2, maxMs = 30000, jitter = 0, random = Math.random } = {}) {
  const n = Math.max(1, attempt);
  const d = Math.min(maxMs, baseMs * Math.pow(factor, n - 1));
  return Math.round(d + (jitter > 0 ? random() * jitter : 0));
}

const defaultSleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Run `fn` up to `attempts` times, sleeping with exponential backoff between
 * retryable failures. Non-retryable errors are thrown immediately.
 */
export async function withRetry(fn, { attempts = 5, baseMs = 1000, maxMs = 30000, jitter = 0, sleep = defaultSleep, shouldRetry = isRetryableError, onRetry = null } = {}) {
  let lastErr;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await fn(attempt);
    } catch (err) {
      lastErr = err;
      if (attempt >= attempts || !shouldRetry(err)) throw err;
      const delay = backoffDelay(attempt, { baseMs, maxMs, jitter });
      if (onRetry) onRetry(err, attempt, delay);
      await sleep(delay);
    }
  }
  throw lastErr;
}

/**
 * @param {object} opts
 * @param {() => Promise<{backendUrl:string, apiToken:string}>|object} opts.getConfig
 * @param {typeof fetch} [opts.fetchFn]
 */
export function createApiClient({ getConfig, fetchFn = null, defaultTimeoutMs = DEFAULT_TIMEOUT_MS } = {}) {
  const doFetch = (...args) => (fetchFn || globalThis.fetch)(...args);

  async function request(method, path, { body, timeoutMs = defaultTimeoutMs, auth = true, signal = null } = {}) {
    const cfg = await getConfig();
    const base = String(cfg.backendUrl || "").replace(/\/+$/, "");
    const headers = { Accept: "application/json" };
    if (auth) {
      if (!cfg.apiToken) throw new ApiError("API token not configured", { code: "not_configured" });
      headers[TOKEN_HEADER] = cfg.apiToken;
    }
    let payload;
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      payload = JSON.stringify(body);
    }

    if (signal && signal.aborted) throw new ApiError("Request aborted", { aborted: true });
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);
    const onAbort = () => controller.abort();
    if (signal) signal.addEventListener("abort", onAbort, { once: true });

    let res;
    try {
      res = await doFetch(base + path, { method, headers, body: payload, signal: controller.signal, cache: "no-store", credentials: "omit" });
    } catch (err) {
      if (timedOut) throw new ApiError(`Request timed out after ${timeoutMs} ms (${method} ${path})`, { timeout: true });
      if (signal && signal.aborted) throw new ApiError("Request aborted", { aborted: true });
      throw new ApiError(`Backend unreachable at ${base}: ${err && err.message ? err.message : err}`, { network: true });
    } finally {
      clearTimeout(timer);
      if (signal) signal.removeEventListener("abort", onAbort);
    }

    let data = null;
    if (res.status !== 204) {
      const text = await res.text().catch(() => "");
      if (text) {
        try {
          data = JSON.parse(text);
        } catch {
          data = text;
        }
      }
    }
    if (!res.ok) {
      const code = data && typeof data === "object" ? data.error ?? null : null;
      const msg = data && typeof data === "object" && data.message ? data.message : `HTTP ${res.status}`;
      throw new ApiError(`${msg} (${method} ${path} -> ${res.status})`, { status: res.status, code, body: data });
    }
    return { status: res.status, data };
  }

  return {
    request,
    health: async () => (await request("GET", "/health", { auth: false, timeoutMs: 5000 })).data,
    status: async () => (await request("GET", "/api/v1/status", { timeoutMs: 5000 })).data,
    submitEmail: (payload) => request("POST", "/api/v1/emails", { body: payload }),
    pendingDrafts: async (waitSeconds = 25, { signal = null } = {}) => {
      const wait = Math.max(0, Math.min(55, Math.floor(waitSeconds)));
      const r = await request("GET", `/api/v1/drafts/pending?wait=${wait}`, { timeoutMs: (wait + 15) * 1000, signal });
      return r.data && typeof r.data === "object" ? r.data : { drafts: [] };
    },
    ackDraft: (draftId, body) => request("POST", `/api/v1/drafts/${encodeURIComponent(draftId)}/ack`, { body }),
  };
}
