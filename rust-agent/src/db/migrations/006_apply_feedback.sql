-- Chrome extension applier: help asked of a person over Telegram, and every
-- failure it reports (the feedback loop for recurring problems).
CREATE TABLE IF NOT EXISTS apply_escalations (
    id                   TEXT PRIMARY KEY,
    link_id              TEXT,
    url                  TEXT NOT NULL,
    title                TEXT,
    company              TEXT,
    platform             TEXT,
    person_id            TEXT,
    kind                 TEXT NOT NULL,
    reason               TEXT NOT NULL,
    status               TEXT NOT NULL,          -- open | skip | done
    answer               TEXT,
    telegram_chat_id     INTEGER,
    telegram_message_id  INTEGER,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_apply_escalations_url ON apply_escalations(url, status);
CREATE INDEX IF NOT EXISTS idx_apply_escalations_tg ON apply_escalations(telegram_message_id);

CREATE TABLE IF NOT EXISTS apply_events (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    link_id     TEXT,
    url         TEXT NOT NULL,
    platform    TEXT,
    person_id   TEXT,
    field       TEXT,
    reason      TEXT NOT NULL,
    detail      TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_apply_events_created ON apply_events(created_at);
