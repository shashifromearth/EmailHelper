import { DEFAULTS } from '../src/api.js';

const form = document.getElementById('form');
const input = (k) => document.getElementById(k);

const stored = { ...DEFAULTS, ...(await chrome.storage.local.get(Object.keys(DEFAULTS))) };
for (const k of Object.keys(DEFAULTS)) {
  if (typeof DEFAULTS[k] === 'boolean') input(k).checked = !!stored[k];
  else input(k).value = Array.isArray(stored[k]) ? stored[k].join(', ') : stored[k];
}

form.onsubmit = async (e) => {
  e.preventDefault();
  const values = {};
  for (const k of Object.keys(DEFAULTS)) {
    if (typeof DEFAULTS[k] === 'boolean') {
      values[k] = input(k).checked;
      continue;
    }
    const v = input(k).value.trim();
    values[k] = Array.isArray(DEFAULTS[k]) ? v.split(/[,\s]+/).filter(Boolean).map((s) => s.toLowerCase()) : v || DEFAULTS[k];
  }
  await chrome.storage.local.set(values);
  input('saved').hidden = false;
  setTimeout(() => (input('saved').hidden = true), 1500);
};
