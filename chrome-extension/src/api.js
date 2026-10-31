// Clients for the local services. `fetchImpl` is injectable for tests.

export const DEFAULTS = {
  agentUrl: 'http://127.0.0.1:7800',
  knowledgeUrl: 'http://127.0.0.1:7820',
  linksUrl: 'http://127.0.0.1:7810',
  apiToken: '', // EH_API_TOKEN: the agent (and the knowledge service unless knowledgeToken is set)
  knowledgeToken: '', // the knowledge service's token; empty = apiToken
  linksToken: '', // JOB_LINKS_TOKEN
  person: '', // claim only this person's jobs; empty = any
  platforms: [], // claim only these platforms; empty = any
  autoSubmit: false, // submit on employer career sites when nothing needs the person
};

/** Fields per answers request: each request must finish well inside MV3's 30 s fetch limit. */
export const ANSWER_BATCH = 6;

export class ApiError extends Error {
  constructor(service, status, message) {
    super(`${service}: ${message}`);
    this.service = service;
    this.status = status;
  }
}

export function createApi(settings, fetchImpl = fetch, timeoutMs = 90_000) {
  const call = async (service, base, header, token, path, { method = 'GET', body, raw = false } = {}) => {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), timeoutMs);
    let res;
    try {
      res = await fetchImpl(base.replace(/\/+$/, '') + path, {
        method,
        headers: { [header]: token, ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: ctrl.signal,
      });
    } catch (e) {
      throw new ApiError(service, 0, e.name === 'AbortError' ? 'timed out' : `not reachable at ${base}`);
    } finally {
      clearTimeout(timer);
    }
    if (!res.ok) {
      let msg = `HTTP ${res.status}`;
      try {
        const j = await res.json();
        msg = j?.error?.message || j?.message || j?.error || msg;
      } catch {
        /* not JSON */
      }
      throw new ApiError(service, res.status, typeof msg === 'string' ? msg : JSON.stringify(msg));
    }
    if (raw) return res;
    return res.status === 204 ? null : res.json();
  };
  const links = (path, opts) => call('job-links', settings.linksUrl, 'x-joblinks-token', settings.linksToken, path, opts);
  const knowledge = (path, opts) =>
    call('knowledge', settings.knowledgeUrl, 'x-knowledge-token', settings.knowledgeToken || settings.apiToken, path, opts);
  const agent = (path, opts) => call('agent', settings.agentUrl, 'x-emailhelper-token', settings.apiToken, path, opts);

  return {
    /** Next queued job for this browser, or null when the queue is empty. `platforms` null = any. */
    async claim(client, platforms = settings.platforms) {
      const body = { client, limit: 1, lease_secs: 3600 };
      if (settings.person) body.person = settings.person;
      if (platforms?.length) body.platforms = platforms;
      const r = await links('/v1/links/claim', { method: 'POST', body });
      return r.links?.[0] ?? null;
    },
    result: (id, client, status, reason) =>
      links(`/v1/links/${encodeURIComponent(id)}/result`, { method: 'POST', body: { status, reason: reason || null, client } }),
    stats: () => links('/v1/stats'),
    /**
     * Answers for a form's fields, from the agent (profile, memory, RAG + LLM),
     * asked a few fields at a time so no request outlives the service worker.
     */
    async answers(person, job, fields, batch = ANSWER_BATCH) {
      let who = person || null;
      const answers = [];
      for (let i = 0; i < fields.length; i += batch) {
        const r = await agent('/api/v1/apply/answers', { method: 'POST', body: { person: who, job, fields: fields.slice(i, i + batch) } });
        who = r.person ?? who;
        answers.push(...(r.answers || []));
      }
      return { person: who, answers };
    },
    /** Answers the person gave, so the agent reuses them next time. */
    remember: (person, answers) => agent('/api/v1/apply/remember', { method: 'POST', body: { person: person || null, answers } }),
    /** Ask the person on Telegram (via the agent) to fix the page or reply skip. Returns { id, telegram }. */
    escalate: (e) => agent('/api/v1/apply/escalations', { method: 'POST', body: e }),
    /** { id, status: 'open' | 'skip' | 'done', answer } */
    escalation: (id) => agent(`/api/v1/apply/escalations/${encodeURIComponent(id)}`),
    resolveEscalation: (id, status) =>
      agent(`/api/v1/apply/escalations/${encodeURIComponent(id)}/resolve`, { method: 'POST', body: { status } }),
    /** Feedback events (fill_failed, blocked, submit_failed, error, applied, skipped, escalated). */
    events: (events) => agent('/api/v1/apply/events', { method: 'POST', body: { events } }),
    /** The resume the knowledge service picks for these skills, with its bytes. */
    async resume(person, skills) {
      const pick = await knowledge(`/v1/people/${encodeURIComponent(person)}/resume`, {
        method: 'POST',
        body: { skills: skills || [], purpose: 'job_seeking' },
      });
      const path = `/v1/people/${encodeURIComponent(person)}/resumes/${encodeURIComponent(pick.file)}`;
      const res = await knowledge(path, { raw: true });
      return {
        file: pick.file,
        stack: pick.stack,
        type: res.headers.get('content-type') || 'application/octet-stream',
        bytes: new Uint8Array(await res.arrayBuffer()),
      };
    },
  };
}
