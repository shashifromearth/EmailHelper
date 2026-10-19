// Local runtime state shared between background, popup and options through
// storage.local (no runtime messaging needed for reads), plus badge logic.

export const STATE_KEY = "ehState";

export const DEFAULT_STATE = Object.freeze({
  backendReachable: null, // null = unknown yet
  authError: false,
  notConfigured: false,
  outboxDepth: 0,
  emailsSubmitted: 0,
  emailsDuplicate: 0,
  emailsDropped: 0,
  draftsSaved: 0,
  draftsFailed: 0,
  unseenDrafts: 0,
  unseenFailures: 0,
  lastError: null, // { message, at }
  lastDraftAt: null,
  lastPollOkAt: null,
  startedAt: null,
});

export const BADGE_COLORS = Object.freeze({ error: "#d70022", queued: "#b8730a", drafts: "#058b00" });

function capCount(n) {
  return n > 99 ? "99+" : String(n);
}

/**
 * Badge priority: error ("!" red) > queued outgoing e-mails (amber count) >
 * drafts saved since the popup was last opened (green count) > nothing.
 */
export function computeBadge(state) {
  const s = { ...DEFAULT_STATE, ...(state || {}) };
  if (s.notConfigured) return { text: "?", color: BADGE_COLORS.error, title: "EmailHelper: not configured" };
  if (s.authError) return { text: "!", color: BADGE_COLORS.error, title: "EmailHelper: API token rejected" };
  if (s.backendReachable === false) return { text: "!", color: BADGE_COLORS.error, title: "EmailHelper: backend unreachable" };
  if (s.unseenFailures > 0) return { text: "!", color: BADGE_COLORS.error, title: `EmailHelper: ${s.unseenFailures} draft(s) failed` };
  if (s.outboxDepth > 0) return { text: capCount(s.outboxDepth), color: BADGE_COLORS.queued, title: `EmailHelper: ${s.outboxDepth} e-mail(s) waiting for the backend` };
  if (s.unseenDrafts > 0) return { text: capCount(s.unseenDrafts), color: BADGE_COLORS.drafts, title: `EmailHelper: ${s.unseenDrafts} new draft(s)` };
  return { text: "", color: BADGE_COLORS.drafts, title: "EmailHelper" };
}

export class StateStore {
  constructor({ storage, onChange = () => {}, key = STATE_KEY }) {
    this.storage = storage;
    this.onChange = onChange;
    this.key = key;
    this.state = { ...DEFAULT_STATE };
    this._chain = Promise.resolve();
  }

  async load() {
    const got = await this.storage.get(this.key);
    this.state = { ...DEFAULT_STATE, ...((got && got[this.key]) || {}) };
    return this.state;
  }

  get() {
    return this.state;
  }

  /** Apply a patch object or `(state) => patch`; writes are serialised. */
  update(patch) {
    this._chain = this._chain
      .then(async () => {
        const p = typeof patch === "function" ? patch(this.state) : patch;
        this.state = { ...this.state, ...(p || {}) };
        await this.storage.set({ [this.key]: this.state });
        this.onChange(this.state);
      })
      .catch(() => {});
    return this._chain;
  }

  recordError(message, now = Date.now()) {
    return this.update({ lastError: { message: String(message).slice(0, 500), at: now } });
  }
}
