// Extension configuration stored in storage.local under CONFIG_KEY.
// Pure helpers + a thin storage wrapper (storage is injected, so tests use an
// in-memory fake with the same get/set shape as messenger.storage.local).

export const CONFIG_KEY = "ehConfig";
export const DEFAULT_BACKEND_URL = "http://127.0.0.1:7800";
const ALLOWED_HOSTS = new Set(["127.0.0.1", "localhost"]);

export const DEFAULT_CONFIG = Object.freeze({
  backendUrl: DEFAULT_BACKEND_URL,
  apiToken: "",
  notifyOnDraft: true,
  monitorAllFolders: false,
  // accountId -> false to disable. Missing means enabled.
  accounts: {},
  // accountId -> one of ACCOUNT_TYPES, overrides the inferred account_type.
  accountTypeOverrides: {},
});

/**
 * Validate the backend URL. Only http://127.0.0.1 and http://localhost are
 * allowed: the manifest only has host permissions for those, and the backend
 * binds to loopback only (contract: "Nothing is ever exposed externally").
 */
export function validateBackendUrl(input) {
  const raw = typeof input === "string" ? input.trim() : "";
  if (!raw) return { ok: false, error: "Backend URL is required" };
  let u;
  try {
    u = new URL(raw);
  } catch {
    return { ok: false, error: "Backend URL is not a valid URL" };
  }
  if (u.protocol !== "http:") return { ok: false, error: "Backend URL must use http://" };
  if (!ALLOWED_HOSTS.has(u.hostname)) return { ok: false, error: "Backend URL host must be 127.0.0.1 or localhost" };
  if (u.username || u.password) return { ok: false, error: "Backend URL must not contain credentials" };
  return { ok: true, url: u.origin };
}

export function normalizeConfig(raw) {
  const src = raw && typeof raw === "object" ? raw : {};
  const cfg = { ...DEFAULT_CONFIG, accounts: {}, accountTypeOverrides: {} };
  const url = validateBackendUrl(src.backendUrl);
  cfg.backendUrl = url.ok ? url.url : DEFAULT_BACKEND_URL;
  cfg.apiToken = typeof src.apiToken === "string" ? src.apiToken.trim() : "";
  cfg.notifyOnDraft = src.notifyOnDraft !== false;
  cfg.monitorAllFolders = src.monitorAllFolders === true;
  if (src.accounts && typeof src.accounts === "object") {
    for (const [k, v] of Object.entries(src.accounts)) cfg.accounts[k] = v !== false;
  }
  if (src.accountTypeOverrides && typeof src.accountTypeOverrides === "object") {
    for (const [k, v] of Object.entries(src.accountTypeOverrides)) {
      if (typeof v === "string" && v) cfg.accountTypeOverrides[k] = v;
    }
  }
  return cfg;
}

export function isAccountEnabled(cfg, accountId) {
  if (!accountId) return false;
  return !(cfg && cfg.accounts && cfg.accounts[accountId] === false);
}

export async function loadConfig(storage) {
  const got = await storage.get(CONFIG_KEY);
  return normalizeConfig(got ? got[CONFIG_KEY] : undefined);
}

export async function saveConfig(storage, cfg) {
  const normalized = normalizeConfig(cfg);
  await storage.set({ [CONFIG_KEY]: normalized });
  return normalized;
}
