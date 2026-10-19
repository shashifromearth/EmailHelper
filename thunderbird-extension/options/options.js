import { loadConfig, saveConfig, validateBackendUrl, isAccountEnabled } from "../src/config.js";
import { createApiClient } from "../src/api.js";
import { ACCOUNT_TYPES, inferAccountType } from "../src/extract.js";
import { callBackground } from "../src/control.js";

const mx = globalThis.messenger || globalThis.browser;
const $ = (id) => document.getElementById(id);

let config = null;
let accounts = [];

function setResult(id, text, cls = "") {
  const el = $(id);
  el.textContent = text;
  el.className = `result ${cls}`;
}

function renderAccounts() {
  const tbody = $("accounts").querySelector("tbody");
  tbody.textContent = "";
  if (!accounts.length) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 4;
    td.textContent = "No mail accounts found.";
    tr.appendChild(td);
    tbody.appendChild(tr);
    return;
  }
  for (const acc of accounts) {
    const tr = document.createElement("tr");
    tr.dataset.accountId = acc.id;

    const tdEnabled = document.createElement("td");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.className = "acc-enabled";
    cb.checked = isAccountEnabled(config, acc.id);
    cb.setAttribute("aria-label", `Enable ${acc.name}`);
    tdEnabled.appendChild(cb);

    const tdName = document.createElement("td");
    const emails = (acc.identities || []).map((i) => i.email).filter(Boolean);
    tdName.textContent = acc.name || acc.id;
    if (emails.length && !emails.includes(acc.name)) {
      const small = document.createElement("div");
      small.className = "hint";
      small.textContent = emails.join(", ");
      tdName.appendChild(small);
    }

    const tdProto = document.createElement("td");
    tdProto.textContent = acc.type || "";

    const tdType = document.createElement("td");
    const sel = document.createElement("select");
    sel.className = "acc-type";
    const inferred = inferAccountType({ type: acc.type, emails, name: acc.name });
    const auto = document.createElement("option");
    auto.value = "";
    auto.textContent = `auto (${inferred})`;
    sel.appendChild(auto);
    for (const t of ACCOUNT_TYPES) {
      const o = document.createElement("option");
      o.value = t;
      o.textContent = t;
      sel.appendChild(o);
    }
    sel.value = config.accountTypeOverrides[acc.id] || "";
    tdType.appendChild(sel);

    tr.append(tdEnabled, tdName, tdProto, tdType);
    tbody.appendChild(tr);
  }
}

function readForm() {
  const url = validateBackendUrl($("backend-url").value);
  if (!url.ok) throw new Error(url.error);
  const next = {
    ...config,
    backendUrl: url.url,
    apiToken: $("api-token").value.trim(),
    notifyOnDraft: $("notify").checked,
    monitorAllFolders: $("monitor-all").checked,
    accounts: { ...config.accounts },
    accountTypeOverrides: { ...config.accountTypeOverrides },
  };
  for (const tr of $("accounts").querySelectorAll("tbody tr[data-account-id]")) {
    const id = tr.dataset.accountId;
    next.accounts[id] = tr.querySelector(".acc-enabled").checked;
    const t = tr.querySelector(".acc-type").value;
    if (t) next.accountTypeOverrides[id] = t;
    else delete next.accountTypeOverrides[id];
  }
  return next;
}

async function save() {
  try {
    config = await saveConfig(mx.storage.local, readForm());
    $("backend-url").value = config.backendUrl;
    setResult("save-result", "Saved.", "ok");
  } catch (err) {
    setResult("save-result", err.message, "err");
  }
}

async function testConnection() {
  let form;
  try {
    form = readForm();
  } catch (err) {
    setResult("test-result", err.message, "err");
    return;
  }
  $("test").disabled = true;
  setResult("test-result", "Testing…");
  const api = createApiClient({ getConfig: () => form });
  const lines = [];
  let ok = true;
  try {
    const h = await api.health();
    const checks = h && h.checks ? Object.entries(h.checks).map(([k, v]) => `${k}=${v}`).join(", ") : "";
    lines.push(`/health: ${h?.status ?? "?"}${h?.version ? ` (v${h.version})` : ""}${checks ? ` – ${checks}` : ""}`);
  } catch (err) {
    ok = false;
    lines.push(`/health failed: ${err.message}`);
  }
  if (!form.apiToken) {
    ok = false;
    lines.push("/api/v1/status skipped: no API token");
  } else {
    try {
      const s = await api.status();
      lines.push(`/api/v1/status: OK – LLM ${s.llm_provider || "?"}${s.llm_model ? ` / ${s.llm_model}` : ""}, ${s.emails_processed ?? 0} emails processed`);
    } catch (err) {
      ok = false;
      lines.push(err.status === 401 ? "/api/v1/status: token rejected (401)" : `/api/v1/status failed: ${err.message}`);
    }
  }
  setResult("test-result", lines.join("\n"), ok ? "ok" : "err");
  $("test").disabled = false;
}

async function processUnread() {
  $("process-unread").disabled = true;
  setResult("process-result", "Scanning…");
  try {
    const { totals } = await callBackground(mx, "processUnread", { timeoutMs: 10 * 60 * 1000 });
    const parts = [`${totals.scanned} unread scanned in ${totals.accounts} account(s)`];
    for (const k of ["queued", "duplicate", "skipped_junk", "invalid", "error"]) if (totals[k]) parts.push(`${k.replace("_", " ")}: ${totals[k]}`);
    setResult("process-result", parts.join(", "), "ok");
  } catch (err) {
    setResult("process-result", err.message, "err");
  } finally {
    $("process-unread").disabled = false;
  }
}

async function init() {
  config = await loadConfig(mx.storage.local);
  $("backend-url").value = config.backendUrl;
  $("api-token").value = config.apiToken;
  $("notify").checked = config.notifyOnDraft;
  $("monitor-all").checked = config.monitorAllFolders;
  try {
    accounts = (await mx.accounts.list(false)) || [];
  } catch {
    accounts = (await mx.accounts.list()) || [];
  }
  accounts = accounts.filter((a) => a.type !== "none" || (a.identities && a.identities.length));
  renderAccounts();

  $("save").addEventListener("click", save);
  $("test").addEventListener("click", testConnection);
  $("process-unread").addEventListener("click", processUnread);
  $("toggle-token").addEventListener("click", () => {
    const input = $("api-token");
    const show = input.type === "password";
    input.type = show ? "text" : "password";
    $("toggle-token").textContent = show ? "Hide" : "Show";
  });
}

document.addEventListener("DOMContentLoaded", () => {
  init().catch((err) => setResult("save-result", `Failed to load settings: ${err.message}`, "err"));
});
