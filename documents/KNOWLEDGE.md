# Knowledge Folder: People, Mailboxes and Purposes

All personal data lives in `knowledge/`, which is git-ignored and never
committed. A sample in the same layout is in `knowledge.example/`.

## Layout

```
knowledge/
  people.json                       who owns which mailbox, and what each mailbox is for
  shared/                           optional household-wide notes (searched for everyone)
  people/<id>/                      one folder per person (<id> = letters, digits, _ or -)
    profile.json                    name, email, phone, location, links, signature, work authorization…
    private.json                    DOB, PAN, Aadhaar, address, passport + sharing rules (never indexed, never sent to an AI)
    resume.json  skills.json  employment_history.json  education.json  projects.json  certifications.json
    company_profile.md              "about me" text used to ground replies
    tone.md                         writing style for this person's drafts
    *.docx / *.pdf                  resumes may sit here…
    job_seeking/
      preferences.json              target roles, locations, remote, relocation, blacklist
      salary.json                   expected_text, current_text, notice_period, last_working_day
      resumes/                      …or here (also .md/.txt), plus optional resumes.json
      replies/*.md                  how to answer recruiters, interviews, follow-ups
    consulting/   services.md rates.md replies/*.md  + consultant profile .docx/.pdf (or resumes/)
    freelancing/  portfolio.md rates.md availability.md replies/*.md  + freelance profile .docx/.pdf (or resumes/)
```

Any other purpose folder works too (for example `teaching/`); give a mailbox
that purpose in `people.json`.

## people.json

```json
{
  "people": [
    {
      "id": "shashi",
      "name": "Shashi Bhushan Kumar",
      "mailboxes": [
        { "address": "shashilpu@gmail.com",         "purposes": ["job_seeking"] },
        { "address": "shashifromearth@outlook.com", "purposes": ["consulting"] },
        { "address": "shashifromearth@gmail.com",   "purposes": ["job_seeking", "consulting", "freelancing"] }
      ],
      "job_agent": true,
      "telegram_chat_id": null
    }
  ],
  "unassigned_mailboxes": "classify_only",
  "research_person": "shashi"
}
```

| Field | Meaning |
|---|---|
| `mailboxes[].address` | The Thunderbird account address that receives the mail. |
| `mailboxes[].purposes` | `job_seeking`, `consulting`, `freelancing` (aliases: jobs, freelance, consultancy) or any custom name. With one purpose, all mail to that mailbox is treated as that purpose. With several, the agent picks one per email. |
| `job_agent` | Whether job emails to this person start applications (still in review mode). |
| `telegram_chat_id` | Optional. This person's own chat with the *same* bot. When empty, their messages go to the default `TELEGRAM_CHAT_ID`. |
| `unassigned_mailboxes` | `classify_only` (default): mail to unlisted mailboxes is classified but gets no draft and no job actions. The dashboard lists these mailboxes. `default_person` replies as `default_person` instead. |
| `research_person` | Whose profile daily job discovery scores against. |

Without `people.json`, the whole `knowledge/` folder is treated as one
person, which is the legacy layout.

## How an email is handled

1. **Route.** The receiving mailbox is looked up in `people.json`, giving a
   person and that mailbox's purposes. If the account itself is unknown, the
   To/Cc addresses are checked.
2. **Purpose.** A single-purpose mailbox uses its purpose. For multi-purpose
   mailboxes:
   - job mail → `job_seeking`;
   - freelance wording (Upwork, hourly rate, fixed price…) → `freelancing`;
   - consulting wording (engagement, SOW, advisory…) → `consulting`.
3. **Knowledge.** Only that person's root files, that purpose's folder and
   `shared/` are searched. Other people's files are never used.
4. **Facts.** The questions the email asks are answered from that person's
   files:
   - CTC, notice period, last working day, experience, current company,
     location, phone and resume come from the normal profile files;
   - DOB, PAN, Aadhaar, address and passport come from `private.json`.
5. **Draft.** The reply is written as that person, with their signature, and
   saved in Thunderbird from the receiving mailbox.
6. **Unknown facts.** Anything the email asks that isn't on file is asked on
   Telegram and on the dashboard. The answer is stored and reused for later
   emails.

## private.json

```json
{
  "date_of_birth": "1990-01-02",
  "pan": "ABCDE1234F",
  "aadhaar": "",
  "address": "…",
  "share": {
    "date_of_birth": "on_request",
    "address": "on_request",
    "pan": "secure_channel",
    "aadhaar": "secure_channel"
  }
}
```

| Share rule | What the draft does when the email asks for it |
|---|---|
| `on_request` | States the value. The AI only ever sees a placeholder such as `{{DATE_OF_BIRTH}}`, and the real value is filled in locally afterwards. The draft is always marked REVIEW REQUIRED. |
| `secure_channel` | Says it will be shared over a secure channel. This is the default for PAN, Aadhaar, passport and bank details. |
| `never` | Politely declines to share it by email. |

`private.json` is excluded from search indexing and from every prompt, even
with cloud models.

## Resumes and tech stacks

### Stack folders (recommended)

Put each stack's resume in its own folder under `resumes/`. The folder name
decides the stack, so every file can simply be called `resume.docx` or
`resume.pdf`:

```
people/<id>/resumes/dotnet/resume.docx
people/<id>/resumes/java/resume.pdf
people/<id>/resumes/python/resume.docx
people/<id>/resumes/golang/resume.docx
people/<id>/resumes/rust/resume.docx
people/<id>/resumes/polyglot/resume.docx     general resume; used when no stack matches
people/<id>/consulting/resumes/genai/profile.pdf
```

Recognised folder names include `dotnet`, `net`, `csharp`, `java`, `spring`,
`python`, `django`, `fastapi`, `golang`, `go`, `rust`, `node`, `react`,
`genai`, `ai`, `ml`, `aws`, `azure`, `devops` and `polyglot`/`general`. For
each job the agent picks the folder whose stack best matches the job's
skills, and the polyglot resume when nothing matches. File-name hints such as
`Shashi_DotNet_Resume.docx` keep working.

### Older layout: stack in the file name

Name resume files after their stack, for example
`Shashi_DotNet_Resume.docx`, `Resume_GOLANG_PYTHON_AWS.docx` or
`resume_rust.docx`.

### Naming convention

There is no strict naming scheme. The folder decides the purpose; the file
name only decides the stack. A good pattern is
`<Name>_<Purpose>_<Stack...>.docx`:

| What | Where | Example name |
|---|---|---|
| Job-seeking resumes | `people/<id>/` or `people/<id>/job_seeking/resumes/` | `Shashi_Resume_DotNet_Azure.docx` |
| Independent consultant profile | `people/<id>/consulting/` (or `consulting/resumes/`) | `Shashi_Consulting_Profile_GenAI_Architect.docx` |
| Freelance profile | `people/<id>/freelancing/` (or `freelancing/resumes/`) | `Shashi_Freelance_Profile_Python_React.docx` |

- Separate words with `_`, `-`, spaces or dots. Matching ignores case.
- Recognised stack words include `dotnet`/`net`/`csharp`, `python`, `java`,
  `golang`/`go`, `rust`, `node`, `react`, `ai`/`genai`, `aws`, `azure`/`az`,
  `kafka`, `k8s` and `architect`.
- `.docx`, `.pdf`, `.doc` and `.rtf` count as resumes anywhere in these
  folders. `.md`/`.txt` count only inside a `resumes/` folder, so notes such
  as `services.md` or `rates.md` are never mistaken for a resume.
- Never put a resume in `replies/`; that folder holds reply guidance only.

Which document is used:

- **Consulting or freelancing emails** use the documents in that purpose's
  folder. If that folder has none, the general resumes are used.
- **Job applications and recruiter emails** use the general resumes and the
  ones in `job_seeking/`. Consulting and freelance profiles are never
  uploaded to a job application.

Within that set, the agent picks the resume that best matches the job's stack:

- File-name hints count double.
- Skills found inside the `.docx` text also count.
- The default resume is used when nothing matches: the first file without a
  stack in its name, or the one marked `"default": true`. Each purpose
  folder has its own default.

The approval message shows which resume was chosen and why. Recruiter emails
asking for a resume get a note saying which file to attach.

Optional overrides go in `resumes.json` next to the resumes:

```json
[ { "file": "Shashi_Resume_Polyglot_GenAI_Architect.docx", "stack": ["Go", "Rust", "Java"], "default": false } ]
```

To check what the agent sees (people, mailboxes, resumes, and which resume
each sample job would get), run:

```bash
cargo run -p emailhelper --example profile_check -- knowledge
```

## Indexing (RAG)

The knowledge service indexes this folder for the email agent and the Chrome
extension:

- **Structure-aware ETL.** `.docx` is split at heading styles and short bold
  titles, with tables kept as rows. `.pdf` is split at title-like lines,
  `.md` at `#` headings and `.json` by top-level key with key paths kept
  (`jobs[0].company: Acme`). `.txt` is split by paragraphs.
- **Chunks.** About 900 characters with 150 characters of overlap. A chunk
  never crosses a section. Each one is titled "Document › Section" and
  carries metadata: person, purpose, document type (resume, skills,
  experience, compensation, reply guide…) and stack.
- **Embeddings.** Every chunk is embedded by Gemini `gemini-embedding-001`
  (768 dimensions, free tier) and by the local Ollama model
  `nomic-embed-text`. Searches use Gemini when it answers and the local
  model otherwise.
- **Store.** One SQLite file (`data/knowledge.db`) holds the chunks, a
  keyword index (FTS5) and one sqlite-vec vector table per model.
- **Search.** Hybrid: vector similarity, keyword (BM25) and metadata
  filters (person, purpose, document type), merged with reciprocal-rank
  fusion. A requested stack's resume gets a small boost.
- **When it runs.** At startup and then once a day (checked every
  `RAG_ETL_CHECK_HOURS`), plus the **Re-index knowledge** button on the
  dashboard. Only changed files are re-chunked; removed files are dropped.
- **Never indexed:** `private.json`, the `private/` folder and `people.json`.

## Telegram (one bot for everyone)

1. In Telegram, message @BotFather, send `/newbot`, and put the token in
   `.env` as `TELEGRAM_BOT_TOKEN`. Also set `TELEGRAM_ENABLED=true`.
2. Each person who wants alerts opens the bot and sends `/start`.
3. With the agent stopped, run `emailhelper telegram-chats` to list the chat
   IDs.
4. Put the main chat ID in `.env` as `TELEGRAM_CHAT_ID`. Optionally add
   per-person `telegram_chat_id` values in `people.json`.
5. Run `emailhelper telegram-test`. It checks the token with Telegram and
   sends a test message to `TELEGRAM_CHAT_ID`. The token must be the whole
   BotFather value, `<bot id>:<secret>`; the agent warns at startup if it is
   not.

Every message is tagged `[Name · mailbox]`. Reply to a question message, or
start your reply with its `Q-XXXX` reference, to answer it.
