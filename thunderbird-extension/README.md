# EmailHelper Thunderbird extension

A Thunderbird MailExtension (Manifest V2, Thunderbird 115 ESR and 128 ESR) that connects
Thunderbird to the local EmailHelper orchestrator (`http://127.0.0.1:7800`, see
`documents/API.md` section 1).

- New incoming mail is sent to `POST /api/v1/emails`.
- Generated replies are fetched with the long poll `GET /api/v1/drafts/pending?wait=25`
  and **saved as Thunderbird drafts**. You review and send them yourself.

**The extension never sends mail.** It has no code path that calls the compose send API,
and a unit test fails the build if the string appears anywhere in the source.

## Install

### Temporary (development)

1. Start the EmailHelper backend.
2. In Thunderbird open **Tools → Developer Tools → Debug Add-ons**
   (or type `about:debugging` in the address bar), then go to **This Thunderbird**.
3. Click **Load Temporary Add-on…** and pick `thunderbird-extension/manifest.json`.

Temporary add-ons are removed when Thunderbird quits.

### Permanent (XPI)

```sh
cd thunderbird-extension
npm run build        # writes ../dist/emailhelper-thunderbird.xpi (Node built-ins only)
```

In Thunderbird open **Tools → Add-ons and Themes**, click the gear icon, choose
**Install Add-on From File…** and select `dist/emailhelper-thunderbird.xpi`.
The XPI is unsigned. Release builds of Thunderbird accept unsigned add-ons
(unlike Firefox), but if your build or policy blocks them, set
`xpinstall.signatures.required` to `false` in the Config Editor.

## Configuration

The settings page opens automatically after install. You can also open it from
**Add-ons and Themes → EmailHelper → Options** or from the **Settings** button in the toolbar popup.

| Setting | Default | Notes |
|---|---|---|
| Backend URL | `http://127.0.0.1:7800` | Must be `http://127.0.0.1` or `http://localhost` (any port). |
| API token | (empty) | The orchestrator's `EH_API_TOKEN`, sent as `X-EmailHelper-Token`. Nothing is sent until it is set. |
| Draft notification | on | Desktop notification "Draft ready" with the subject. |
| Watch folders other than the Inbox | off | Thunderbird 121+ only. Also reports mail that filters move into other folders. |
| Accounts | all enabled | Untick an account to stop sending its mail. The account type (`gmail`, `outlook`, `zoho`, `imap`, `pop3`, `other`) can be overridden per account. |

**Test connection** calls `/health` and `/api/v1/status` with the values in the form,
before you save them. **Process unread Inbox mail** queues the unread messages already
in each enabled account's Inbox (up to 200 per account). Messages already handed to the
backend are skipped.

Everything is stored in `storage.local`: settings, outbox, dedupe set, saved-draft
records and status counters.

## Toolbar popup and badge

The popup shows backend health (from `/health` checks), LLM provider and model, the backend
queue depth, emails processed, drafts created and pending, review-required count, backend
failures, how many e-mails are waiting locally for upload, and the last error. It also
has **Open dashboard**, which opens `<backend>/dashboard` in your default browser.

Badge:

| Badge | Meaning |
|---|---|
| red `?` | Not configured (no API token). |
| red `!` | Backend unreachable, token rejected, or a draft failed since you last opened the popup. |
| amber number | E-mails queued locally because the backend is unavailable. |
| green number | Drafts saved since you last opened the popup. |

## How it works

### New mail → backend

1. `messages.onNewMailReceived` fires for a folder and a `MessageList`. All pages are read
   through `messages.continueList`.
2. Mail is skipped if it is in a Junk, Trash, Sent, Drafts, Templates or Outbox folder
   (checked with `folder.type` on TB115, `folder.specialUse` on TB121+, and well-known
   folder names as a fallback), if it is flagged as junk, or if the account is disabled.
3. A dedupe key `accountId|<message-id>` is checked against an LRU set (5000 entries,
   persisted). The backend also dedupes.
4. `messages.getFull` provides headers (`message-id`, `in-reply-to`, `references`, `to`,
   `cc`, `date`) and the body. `text/plain` parts are used when present. Otherwise HTML is
   converted to text. Attachment metadata comes from `messages.listAttachments` (falling
   back to walking the MIME tree), and attachment content is never read. `body_text` is
   truncated to 64 KiB (UTF-8 safe) and `subject` to 998 characters.
5. The payload goes into the **outbox** in `storage.local` first, then it is POSTed.
   Each item gets up to 5 attempts with exponential backoff (1 s, 2 s, 4 s, 8 s). If the
   backend stays down, the queue is kept, also across restarts, and retried every 60 s.
   It is also retried as soon as the draft long poll reaches the backend again.
   Permanent 4xx rejections are dropped and logged so one bad message cannot block the
   queue. A 401 stops delivery until the token is fixed.

### Drafts ← backend

A single sequential loop runs `GET /api/v1/drafts/pending?wait=25`. A new request starts
only after the previous one has finished and its drafts are processed. On a network error
it backs off 5 s → 10 s → 20 s → 40 s → 60 s, and resets after the first success.

For each draft, one at a time (a mutex makes sure only one compose window is open):

1. `kind = "reply"`: the extension checks that `tb_message_id` still points to the message
   with the draft's `message_id` **in the draft's `account_id`**, or finds it again by
   Message-ID in that same account. Then it calls
   `compose.beginReply(id, "replyToSender", …)`, so Thunderbird sets `In-Reply-To` and
   `References` and keeps the thread intact.
   `kind = "new"`: `compose.beginNew({ to, subject, identityId, … })`.
2. **The reply always comes from the mailbox that received the email.** The sender is the
   identity of `account_id` whose address equals the draft's `from_address`
   (case-insensitive), or the account's default identity when `from_address` is null or
   not an identity of that account (a warning is logged). For replies it is set
   explicitly with `setComposeDetails({ identityId })` instead of leaving the choice to
   Thunderbird. Identities of other accounts are never used. When the same mail was
   BCC'd to several mailboxes, copies in other accounts are never used as the reply
   target. If the account has no copy, the draft is acked `failed`.
3. The body format follows the identity. HTML identities (`composeHtml`) get an escaped
   HTML `body`. Plain-text identities get `plainTextBody`. If the window opens in the other
   format, the body is rewritten with `setComposeDetails`.
4. `validation = "REVIEW_REQUIRED"` adds this first line:
   `[EmailHelper: REVIEW REQUIRED – <notes joined by "; ">]`.
5. `compose.saveMessage(tabId, { mode: "draft" })` (retried once), then the compose window
   is closed.
6. `POST /api/v1/drafts/{id}/ack` sends `{status:"saved", tb_draft_message_id}` or
   `{status:"failed", error}`. Acks that cannot be delivered are kept in `storage.local`
   and retried before the next poll. Saved draft ids are remembered, so a draft that the
   backend hands out again (after a lost ack) is re-acked instead of being saved twice.

## Permissions

| Permission | Why |
|---|---|
| `messagesRead` | `onNewMailReceived`, `getFull`, `listAttachments`, `list`/`query` to find the message to reply to. |
| `accountsRead` | Account names, types and identities (account id, e-mail, account type, sender identity for drafts). |
| `compose` | Open reply and new-message compose windows. |
| `compose.save` | Save the compose window as a draft. |
| `storage` | Settings, outbox, dedupe set and status in `storage.local`. |
| `notifications` | Optional "Draft ready" notification. |
| `http://127.0.0.1/*`, `http://localhost/*` | Talk to the local backend. No other hosts are contacted. |

## Development

```sh
cd thunderbird-extension
npm test             # node --test tests/*.test.js  (no dependencies, Node 22+ recommended)
npm run build        # ../dist/emailhelper-thunderbird.xpi
```

Layout:

```
manifest.json         MV2 manifest
background.html/.js   background page (ES module) that wires everything to messenger.*
src/extract.js        pure: MIME tree, HTML→text, header parsing, account type, payload
src/api.js            fetch wrapper (timeout, token header, ApiError, retry/backoff)
src/outbox.js         durable FIFO outbox with retry
src/dedupe.js         persisted LRU dedupe set
src/drafts.js         compose/save/ack processor and long-poll loop
src/ingest.js         new-mail handling (messenger injected)
src/config.js         settings
src/status.js         runtime state and badge logic
src/control.js        popup/options → background port requests
popup/, options/      UI
tests/                node:test suites with fake fetch / fake messenger
scripts/build.mjs     dependency-free ZIP/XPI writer
```

Logs appear in the add-on's console (`about:debugging` → EmailHelper → **Inspect**),
prefixed with `[EmailHelper]`.

## Limitations

- **Thunderbird must be running.** Mail is only detected and drafts are only saved while
  Thunderbird is open. Drafts generated in the meantime wait in the backend and are saved
  on the next start.
- **The compose window flashes briefly** for every saved draft. The MailExtension API can
  only save drafts through a compose window, so the window opens, saves and closes.
  Drafts are processed one at a time to avoid a burst of windows.
- A reply draft replaces Thunderbird's default reply body, including the quoted original.
  Threading headers are still set by Thunderbird.
- The MailExtension API does not expose the incoming server hostname. The account type is
  inferred from identity e-mail domains and the account name (Thunderbird defaults it to the
  e-mail address), then from the protocol. Google Workspace or Microsoft 365 on a custom
  domain shows up as `imap`. Override it in the settings.
- By default only mail that Thunderbird reports as new (normally Inbox) is seen. Enable
  "watch other folders" on Thunderbird 121+ for mail moved by filters.
- Messages that arrive while Thunderbird is closed are reported by Thunderbird as new at
  the next sync. Use **Process unread Inbox mail** to catch up on anything older.
- Thunderbird's `tb_message_id` values are not guaranteed stable across restarts or
  moves. Replies are matched by Message-ID when the id no longer fits.
