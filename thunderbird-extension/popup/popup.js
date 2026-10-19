import { loadConfig } from "../src/config.js";
import { createApiClient } from "../src/api.js";
import { STATE_KEY, DEFAULT_STATE } from "../src/status.js";
import { callBackground } from "../src/control.js";

const mx = globalThis.messenger || globalThis.browser;
const $ = (id) => document.getElementById(id);

function setText(id, value) {
  $(id).textContent = value === null || value === undefined || value === "" ? "–" : String(value);
}

function setHealth(text, cls, title = "") {
  const el = $("health");
  el.textContent = text;
  el.className = `pill ${cls}`;
  el.title = title;
}

function fmtTime(ts) {
  if (!ts) return "";
  const d = new Date(ts);
  return d.toLocaleString();
}

function renderLocalState(s) {
  const st = { ...DEFAULT_STATE, ...(s || {}) };
  setText("outbox", st.outboxDepth);
  setText("local-drafts", `${st.draftsSaved}${st.draftsFailed ? ` (${st.draftsFailed} failed)` : ""}`);
  if (st.lastError && st.lastError.message) {
    $("last-error").hidden = false;
    $("last-error-msg").textContent = st.lastError.message;
    $("last-error-time").textContent = fmtTime(st.lastError.at);
  } else {
    $("last-error").hidden = true;
  }
}

async function readLocalState() {
  const got = await mx.storage.local.get(STATE_KEY);
  renderLocalState(got[STATE_KEY]);
}

async function refreshBackend() {
  const cfg = await loadConfig(mx.storage.local);
  $("config-warning").hidden = !!cfg.apiToken;
  const api = createApiClient({ getConfig: () => cfg });

  try {
    const health = await api.health();
    const checks = health && health.checks ? health.checks : {};
    const degraded = Object.entries(checks).filter(([, v]) => v !== "ok" && v !== "disabled");
    const title = Object.entries(checks).map(([k, v]) => `${k}: ${v}`).join("\n");
    if (health && health.status === "ok" && !degraded.length) setHealth("backend ok", "ok", title);
    else setHealth(degraded.length ? `degraded: ${degraded.map(([k]) => k).join(", ")}` : String(health?.status || "unknown"), "warn", title);
  } catch (err) {
    setHealth("backend down", "err", err.message);
  }

  if (!cfg.apiToken) return;
  try {
    const s = await api.status();
    setText("llm", s.llm_provider ? `${s.llm_provider}${s.llm_model ? ` / ${s.llm_model}` : ""}` : "");
    $("llm").title = $("llm").textContent;
    setText("version", s.version);
    setText("queue", s.queue_depth);
    setText("processed", s.emails_processed);
    setText("drafts-created", s.drafts_created);
    setText("drafts-pending", s.drafts_pending);
    setText("review", s.review_required);
    setText("failures", s.failures);
  } catch (err) {
    if (err.status === 401 || err.status === 403) setHealth("token rejected", "err", err.message);
  }
}

async function openDashboard() {
  const cfg = await loadConfig(mx.storage.local);
  const url = `${cfg.backendUrl}/dashboard`;
  try {
    await mx.windows.openDefaultBrowser(url);
  } catch {
    await mx.tabs.create({ url });
  }
  window.close();
}

function openOptions(e) {
  if (e) e.preventDefault();
  mx.runtime.openOptionsPage();
  window.close();
}

document.addEventListener("DOMContentLoaded", () => {
  $("open-dashboard").addEventListener("click", openDashboard);
  $("open-options").addEventListener("click", openOptions);
  $("open-options-link").addEventListener("click", openOptions);
  $("clear-error").addEventListener("click", () => callBackground(mx, "clearError").catch(() => {}));
  $("retry").addEventListener("click", async () => {
    $("retry").disabled = true;
    try {
      await callBackground(mx, "flushOutbox");
    } catch {
      /* state shows the error */
    }
    await refreshBackend();
    $("retry").disabled = false;
  });

  mx.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes[STATE_KEY]) renderLocalState(changes[STATE_KEY].newValue);
  });

  readLocalState();
  refreshBackend();
  callBackground(mx, "popupOpened", { timeoutMs: 5000 }).catch(() => {}); // clears the "new drafts" badge
});
