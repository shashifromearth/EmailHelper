-- LLM-as-judge scores for a sample of real drafts (EVAL_LIVE_SAMPLE).
CREATE TABLE IF NOT EXISTS eval_live (
    id            INTEGER PRIMARY KEY,
    draft_id      TEXT NOT NULL,
    ts            TEXT NOT NULL,
    groundedness  REAL NOT NULL,
    relevance     REAL NOT NULL,
    faithfulness  REAL NOT NULL,
    abstained     INTEGER NOT NULL,
    notes         TEXT
);
CREATE INDEX IF NOT EXISTS idx_eval_live_ts ON eval_live(ts);
