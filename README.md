# Rust Job Watcher

This service renders the public 104 Rust search in Chromium/CDP, compares
normalized jobs with the authoritative local `jobs.sqlite3`, exports daily
changes, and sends LINE digests. It does not use undocumented 104 HTTP
endpoints directly or bypass CAPTCHA, Cloudflare, authentication, or access
controls.

## Runtime behavior

Axum listens on port `3004` by default and accepts `POST /webhook`. Browser,
SQLite, filesystem, rclone, and LINE work runs off the Tokio executor.

The service synchronizes once at startup and automatically every day at
`06:30 Asia/Taipei`. Automatic `17:00` and `21:30` runs are removed.

LINE commands:

- `更新JD` runs one guarded synchronization immediately and returns its digest.
- `今日履歷` reads today's existing `changes/YYYY-MM-DD.json`; it never opens
  104. Repeated changes for one `(source, external_id)` are presented once
  using the latest record.
- `url` returns today's configured private Google Drive URL or authenticated
  rclone path without starting a synchronization.

Unknown LINE messages are ignored. If no history exists for today, the reply is
`今日尚無職缺異動。`.

## Local one-shot update binary

Use the separate `update-jobs` binary when LINE and cloud upload must not be
opened:

```bash
cargo run --bin update-jobs
```

It performs exactly one 104 synchronization, updates local `jobs.sqlite3`,
`job-list.json`, `job-list.html`, local change history, and only changed
full-JD batch files under `changes/` (or `JOB_WATCHER_JD_DIR`). Files are
written incrementally after every 10 JDs and each batch contains at most 10
complete JDs. They are placed in a date folder such as `changes/08-16/`. It uses the
recent-first list and fetches details for new or potentially changed listings.

To refresh every full JD file, use:

```bash
cargo run --bin update-jobs-all
```

Both one-shot binaries exit after one local update and do not initialize the
Axum server or scheduler. Set `JOB_WATCHER_LINE_BOT=true` to send the completion
digest through LINE and enable the configured cloud upload; when false, they
only write local files and tracing logs. JD batch files are named
`jd-<run>-<chunk>.json` inside the date folder and are removed when their age
exceeds seven days. The normal `job-watcher` binary
enables the LINE bot, scheduler, and configured private Drive export.

## Acquisition and state

Search cards provide summary fields. New and known jobs are opened in a second
normal Chromium tab and the complete visible detail text is extracted. A detail
failure is logged; a previous valid JD is preserved and empty failed content
never replaces it. Incremental mode uses recent-first listing fields as a
pre-filter; `update-jobs-all` is available when every JD must be checked. See
[docs/104-source.md](docs/104-source.md).

SQLite remains the only current-state store. Its identity is `(source,
external_id)`, and existing databases are migrated in place. Deterministic
comparison reports New/Updated/Deleted/Unchanged; textual comparison ignores
line-ending and insignificant whitespace differences. Updated history includes
`changed_fields` and the latest complete JD.

## Change exports and Google Drive

Each successful synchronization appends a run to `changes/YYYY-MM-DD.json`.
The JSON contains `date` and `runs`; each run has `generated_at`, `trigger`, and
`new`, `updated`, `deleted` arrays. New/updated records contain complete
normalized data and `description`; deleted records contain identity, summary,
URL, and deletion timestamp. The current day and previous six calendar days
are retained. Earlier runs on the same day are never overwritten.

When `JOB_WATCHER_RCLONE_REMOTE` is configured, the service runs:

```bash
rclone sync "$JOB_WATCHER_CHANGE_DIR" \
  "$JOB_WATCHER_RCLONE_REMOTE:$JOB_WATCHER_RCLONE_PATH" \
  [--config "$JOB_WATCHER_RCLONE_CONFIG"]
```

Only the configured change directory is synchronized; `jobs.sqlite3` is never
uploaded. Failed uploads leave local JSON and are reported by LINE. No public
Drive sharing is enabled; the notification shows the private authenticated
Drive path. Settings are `JOB_WATCHER_CHANGE_DIR`,
`JOB_WATCHER_RCLONE_REMOTE`, `JOB_WATCHER_RCLONE_PATH`, and optional
`JOB_WATCHER_RCLONE_CONFIG`. `JOB_WATCHER_DRIVE_URL` may contain a real private
Google Drive URL for the daily change file; it is never generated as a fake
public URL. `JOB_WATCHER_JD_DIR` controls the local full-JD batch folder and
defaults to `changes`. The daemon uses the same date-folder and 10-JD batch
layout as the one-shot updater.

## Raspberry Pi / rclone

```bash
sudo apt install rclone
rclone config
rclone lsd gdrive:
```

Configure a private Google Drive remote. The systemd user must read the same
rclone config; set `JOB_WATCHER_RCLONE_CONFIG` explicitly because systemd may
have a different `HOME` than an interactive shell. Do not grant “Anyone with
the link”. Install Chromium, Rust, and CJK fonts as described in
[install.md](install.md), then start Chromium with remote debugging on `9222`.

## Verification

```bash
cargo fmt --check
cargo clippy --offline --locked --all-targets --all-features -- -D warnings
cargo test --offline --locked --all-features
```
