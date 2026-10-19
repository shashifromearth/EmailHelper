-- Employer job boards found for companies seen in platform alerts
-- (LinkedIn/Naukri). ats/slug NULL = no public Greenhouse/Lever/Ashby board.
CREATE TABLE IF NOT EXISTS employer_boards (
    company_key TEXT PRIMARY KEY,
    ats TEXT,
    slug TEXT,
    checked_at TEXT NOT NULL
);
-- Jobs that can only be applied to on a platform go to a daily Telegram digest.
ALTER TABLE jobs ADD COLUMN digest_sent_at TEXT;
