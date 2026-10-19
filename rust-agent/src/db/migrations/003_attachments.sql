-- Files Thunderbird attaches to a draft (JSON list of absolute paths
-- inside the knowledge folder, e.g. the resume chosen for the reply).
ALTER TABLE drafts ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]';
