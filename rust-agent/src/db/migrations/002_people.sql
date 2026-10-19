-- Multi-person routing: who an email/job/application belongs to, which
-- purpose it was handled under, and which mailbox a draft must be sent from.

ALTER TABLE emails ADD COLUMN person_id TEXT;
ALTER TABLE emails ADD COLUMN purpose TEXT;
CREATE INDEX idx_emails_person ON emails(person_id);

ALTER TABLE drafts ADD COLUMN from_address TEXT;
ALTER TABLE drafts ADD COLUMN person_id TEXT;

ALTER TABLE jobs ADD COLUMN person_id TEXT;
ALTER TABLE applications ADD COLUMN person_id TEXT;
ALTER TABLE applications ADD COLUMN resume_file TEXT;

-- Facts a person supplied through Telegram/dashboard (e.g. notice period).
CREATE TABLE person_facts (
    person_id   TEXT NOT NULL,
    fact        TEXT NOT NULL,
    value       TEXT NOT NULL,
    source      TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (person_id, fact)
);
