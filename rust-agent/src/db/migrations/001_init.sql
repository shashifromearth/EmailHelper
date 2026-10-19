-- EmailHelper schema v1

CREATE TABLE emails (
    id               TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL,
    account_name     TEXT,
    account_email    TEXT,
    account_type     TEXT NOT NULL DEFAULT 'imap',
    message_id       TEXT NOT NULL,
    tb_message_id    INTEGER NOT NULL,
    folder           TEXT,
    in_reply_to      TEXT,
    refs             TEXT NOT NULL DEFAULT '[]',
    from_addr        TEXT NOT NULL,
    from_email       TEXT NOT NULL DEFAULT '',
    to_addrs         TEXT NOT NULL DEFAULT '[]',
    cc_addrs         TEXT NOT NULL DEFAULT '[]',
    subject          TEXT NOT NULL,
    received_at      TEXT,
    body_text        TEXT NOT NULL,
    body_html        TEXT,
    body_hash        TEXT NOT NULL,
    attachments      TEXT NOT NULL DEFAULT '[]',
    category         TEXT,
    classification   TEXT,
    is_job           INTEGER NOT NULL DEFAULT 0,
    status           TEXT NOT NULL DEFAULT 'received',
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE (account_id, message_id)
);
CREATE INDEX idx_emails_from ON emails(from_email, created_at);
CREATE INDEX idx_emails_created ON emails(created_at);

-- Durable work queue. idempotency_key = kind:account:message:workflow_version etc.
CREATE TABLE processing_jobs (
    id               TEXT PRIMARY KEY,
    kind             TEXT NOT NULL,
    idempotency_key  TEXT NOT NULL UNIQUE,
    ref_id           TEXT,
    payload          TEXT,
    status           TEXT NOT NULL,             -- queued | running | done | dead
    attempts         INTEGER NOT NULL DEFAULT 0,
    max_attempts     INTEGER NOT NULL DEFAULT 3,
    last_error       TEXT,
    run_after        TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX idx_processing_status ON processing_jobs(status, run_after);

CREATE TABLE drafts (
    id                  TEXT PRIMARY KEY,
    email_id            TEXT REFERENCES emails(id),
    kind                TEXT NOT NULL,           -- reply | new
    account_id          TEXT NOT NULL,
    tb_message_id       INTEGER,
    message_id          TEXT,
    to_addrs            TEXT NOT NULL DEFAULT '[]',
    subject             TEXT NOT NULL,
    body                TEXT NOT NULL,
    validation          TEXT NOT NULL,           -- APPROVED | REVIEW_REQUIRED
    notes               TEXT NOT NULL DEFAULT '[]',
    confidence          REAL,
    status              TEXT NOT NULL,           -- pending | delivered | saved | failed
    delivery_attempts   INTEGER NOT NULL DEFAULT 0,
    delivered_at        TEXT,
    tb_draft_message_id INTEGER,
    error               TEXT,
    llm_provider        TEXT,
    llm_model           TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX idx_drafts_status ON drafts(status, created_at);

CREATE TABLE audit_logs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           TEXT NOT NULL,
    actor        TEXT NOT NULL,
    action       TEXT NOT NULL,
    entity_type  TEXT,
    entity_id    TEXT,
    details      TEXT
);
CREATE INDEX idx_audit_entity ON audit_logs(entity_type, entity_id);
CREATE INDEX idx_audit_ts ON audit_logs(ts);

CREATE TABLE settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE VIEW config AS SELECT key, value, updated_at FROM settings;

CREATE TABLE knowledge_registry (
    path        TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    mtime       INTEGER NOT NULL,
    hash        TEXT NOT NULL,
    chunks      INTEGER NOT NULL DEFAULT 0,
    indexed_at  TEXT NOT NULL
);
CREATE VIRTUAL TABLE knowledge_fts USING fts5(
    path UNINDEXED,
    title,
    content,
    tokenize = 'porter unicode61'
);

CREATE TABLE jobs (
    id                TEXT PRIMARY KEY,
    fingerprint       TEXT NOT NULL UNIQUE,
    source            TEXT NOT NULL,
    source_ref        TEXT,
    company           TEXT,
    title             TEXT NOT NULL,
    url               TEXT,
    location          TEXT,
    remote            TEXT,
    salary            TEXT,
    salary_min        REAL,
    salary_max        REAL,
    salary_currency   TEXT,
    experience        TEXT,
    skills            TEXT NOT NULL DEFAULT '[]',
    description       TEXT,
    score             INTEGER,
    score_breakdown   TEXT,
    decision          TEXT,
    decision_reasons  TEXT NOT NULL DEFAULT '[]',
    status            TEXT NOT NULL DEFAULT 'new',
    discovered_at     TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);
CREATE INDEX idx_jobs_score ON jobs(score DESC);
CREATE INDEX idx_jobs_discovered ON jobs(discovered_at);

CREATE TABLE applications (
    id                 TEXT PRIMARY KEY,
    job_id             TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    company            TEXT,
    role               TEXT,
    url                TEXT NOT NULL,
    status             TEXT NOT NULL,
    current_step       TEXT,
    page_url           TEXT,
    ats                TEXT,
    fields             TEXT NOT NULL DEFAULT '[]',
    answers            TEXT NOT NULL DEFAULT '{}',
    resume_uploaded    INTEGER NOT NULL DEFAULT 0,
    cover_letter       TEXT,
    approved           INTEGER NOT NULL DEFAULT 0,
    blocker_kind       TEXT,
    blocker_detail     TEXT,
    attempts           INTEGER NOT NULL DEFAULT 0,
    run_seq            INTEGER NOT NULL DEFAULT 0,
    score              INTEGER,
    confirmation_text  TEXT,
    screenshot_path    TEXT,
    applied_at         TEXT,
    notes              TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX idx_applications_status ON applications(status);

CREATE TABLE application_status (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    application_id  TEXT NOT NULL REFERENCES applications(id),
    status          TEXT NOT NULL,
    detail          TEXT,
    ts              TEXT NOT NULL
);
CREATE INDEX idx_app_status_app ON application_status(application_id);

CREATE TABLE human_questions (
    id                   TEXT PRIMARY KEY,
    ref_code             TEXT NOT NULL UNIQUE,
    application_id       TEXT REFERENCES applications(id),
    field_id             TEXT,
    question             TEXT NOT NULL,
    reason               TEXT NOT NULL,
    options              TEXT NOT NULL DEFAULT '[]',
    status               TEXT NOT NULL,          -- open | answered | cancelled
    telegram_message_id  INTEGER,
    answer               TEXT,
    created_at           TEXT NOT NULL,
    answered_at          TEXT
);
CREATE INDEX idx_questions_status ON human_questions(status);
CREATE INDEX idx_questions_tg ON human_questions(telegram_message_id);

-- Answers the user gave once, reused for identical questions later.
CREATE TABLE answer_memory (
    question_key  TEXT PRIMARY KEY,
    question      TEXT NOT NULL,
    answer        TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE TABLE recruiters (
    id              TEXT PRIMARY KEY,
    company         TEXT NOT NULL,
    name            TEXT NOT NULL,
    title           TEXT,
    linkedin_url    TEXT,
    email           TEXT,
    source          TEXT NOT NULL,
    rank_score      INTEGER NOT NULL DEFAULT 0,
    outreach_draft  TEXT,
    status          TEXT NOT NULL DEFAULT 'new',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (company, name)
);

CREATE TABLE interviews (
    id             TEXT PRIMARY KEY,
    email_id       TEXT REFERENCES emails(id),
    company        TEXT,
    role           TEXT,
    scheduled_for  TEXT,
    notes          TEXT,
    created_at     TEXT NOT NULL
);

CREATE TABLE research_findings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    query       TEXT NOT NULL,
    url         TEXT NOT NULL UNIQUE,
    title       TEXT,
    snippet     TEXT,
    source      TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
