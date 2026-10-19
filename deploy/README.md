# Native deployment

Build first: `cargo build --release -p emailhelper`. The output is
`target/release/emailhelper`, or `emailhelper.exe` on Windows. The agent
always binds to `127.0.0.1:7800` and the worker to `127.0.0.1:7801`.

## Linux, system service (systemd)

```sh
sudo deploy/linux/install.sh                       # agent only
sudo deploy/linux/install.sh --with-browser-worker # + Node/Playwright worker
sudo $EDITOR /etc/emailhelper/emailhelper.env      # set EH_API_TOKEN, BROWSER_WORKER_TOKEN, ...
sudo systemctl restart emailhelper
journalctl -u emailhelper -f
```

The script is idempotent, so re-run it to upgrade. It always replaces the
binary and the unit files. It never overwrites `/etc/emailhelper/emailhelper.env`,
`/etc/emailhelper/knowledge`, `/etc/emailhelper/config` or `/var/lib/emailhelper`.

| Path | Purpose |
|---|---|
| `/usr/local/bin/emailhelper` | binary |
| `/etc/emailhelper/emailhelper.env` | environment, created from `.env.example` with absolute paths appended (`root:emailhelper 0640`) |
| `/etc/emailhelper/knowledge`, `/etc/emailhelper/config` | read-only for the service |
| `/var/lib/emailhelper` | database, browser profile, screenshots (the only writable path) |
| `/opt/emailhelper/browser-worker`, `/opt/emailhelper/ms-playwright` | worker code and Chromium |

`emailhelper.service` runs as the unprivileged `emailhelper` user with
`ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `NoNewPrivileges`, no
capabilities, a syscall filter and `MemoryDenyWriteExecute`. It runs
`emailhelper migrate` before `serve` and uses `Restart=on-failure`. The worker
unit is a little looser, because V8 and Chromium need W^X exceptions and
namespaces. Check the effect with `systemd-analyze security emailhelper`.

## Linux, user service (`systemctl --user`)

Thunderbird runs in your desktop session, so you may prefer running the agent
as yourself, with knowledge and data in your home directory and no root
needed. For the worker this also allows a visible (headful) browser for
one-time logins.

```sh
install -Dm0755 target/release/emailhelper ~/.local/bin/emailhelper
install -Dm0600 .env ~/.config/emailhelper/emailhelper.env
cp -r knowledge config ~/.config/emailhelper/
install -Dm0644 deploy/linux/user/emailhelper.service ~/.config/systemd/user/emailhelper.service
systemctl --user daemon-reload
systemctl --user enable --now emailhelper.service
loginctl enable-linger "$USER"   # optional: keep running when logged out
journalctl --user -u emailhelper -f
```

The unit sets `EH_DATA_DIR=~/.local/share/emailhelper` and
`EH_KNOWLEDGE_DIR=~/.config/emailhelper/knowledge`. Values in the env file take
precedence, so remove or adjust relative paths such as `EH_KNOWLEDGE_DIR=./knowledge`
there. The working directory is `~/.local/share/emailhelper`. The optional
worker unit `deploy/linux/user/emailhelper-browser-worker.service` has its
install steps in its header. Don't run the system and user variants at the
same time, because both use port 7800.

## Windows

Run from a normal (non-elevated) PowerShell in the repo root:

```powershell
cargo build --release -p emailhelper
powershell -ExecutionPolicy Bypass -File deploy\windows\install.ps1                     # agent
powershell -ExecutionPolicy Bypass -File deploy\windows\install.ps1 -WithBrowserWorker  # + worker
notepad $env:LOCALAPPDATA\EmailHelper\.env       # set EH_API_TOKEN etc., then:
Start-ScheduledTask -TaskName 'EmailHelper Agent'
```

- Installs to `%LOCALAPPDATA%\EmailHelper`: `emailhelper.exe`, `knowledge\`,
  `config\` (only missing files are copied), `.env` (created from
  `.env.example` only if absent), `data\`, `logs\`, `run-hidden.ps1` and
  `uninstall.ps1`.
- Registers the per-user task **EmailHelper Agent**, which starts at logon,
  runs hidden with the install directory as working directory, and restarts on
  failure 3 times, 1 minute apart. With `-WithBrowserWorker` it also registers
  **EmailHelper Browser Worker**, which runs `node browser-worker\src\index.js`.
  `npm ci` and `playwright install chromium` run unless you pass `-SkipNpmInstall`.
- Tasks start through `run-hidden.ps1`, so no console window appears. Output
  goes to `logs\agent.out.log` / `.err.log`, with the previous run kept as
  `.1`. The launcher also does the 3×/60 s restart, because Task Scheduler's
  own restart setting doesn't fire when a program exits with an error code.
- Re-running the installer upgrades in place. It stops the task, replaces the
  exe and runs `emailhelper migrate`.
- If `Register-ScheduledTask` says *Access is denied* (some group policies),
  run the installer elevated. The task still runs as you.

`-AsService` (elevated, needs `nssm.exe` on PATH) installs Windows services
`EmailHelper` and `EmailHelperBrowserWorker` via NSSM instead. They run as
`NT AUTHORITY\LocalService` (change with `-ServiceAccount`) with log rotation
and restart-on-exit. **Services run in session 0**: no desktop, no headful
browser, and not as your user. They also use the elevated user's
`%LOCALAPPDATA%`. Scheduled-task mode is recommended on a desktop that runs
Thunderbird.

Uninstall:

```powershell
powershell -ExecutionPolicy Bypass -File $env:LOCALAPPDATA\EmailHelper\uninstall.ps1             # keep data
powershell -ExecutionPolicy Bypass -File $env:LOCALAPPDATA\EmailHelper\uninstall.ps1 -RemoveData # delete everything
```

To remove services, run uninstall from an elevated shell. Both scripts target
Windows PowerShell 5.1.

Monitoring for a native agent: see `docker/docker-compose.monitoring.yml` and
`monitoring/README.md`.
