# Rust Job Watcher

This service renders the public 104 Rust search in Chromium/CDP and optionally
acquires LinkedIn jobs from Gmail Job Alert messages, compares normalized jobs with the
authoritative local `jobs.sqlite3`, exports daily changes, and sends LINE/Gmail
digests. It does not use undocumented 104 HTTP
endpoints directly or bypass CAPTCHA, Cloudflare, authentication, or access
controls.

## Runtime behavior

Axum listens on port `3004` by default and accepts `POST /webhook`. Browser,
SQLite, filesystem, SMTP, and LINE work runs off the Tokio executor.

The service synchronizes all enabled sources once at startup by default and
automatically every day at `06:30 Asia/Taipei`. Set
`JOB_WATCHER_SYNC_ON_STARTUP=false` to skip the immediate startup run.
Automatic `17:00` and `21:30` runs are removed.

LINE commands:

- `更新JD` runs one guarded synchronization immediately and returns its digest.
- `今日履歷` reads today's existing `changes/YYYY-MM-DD.json`; it never opens
  104. Repeated changes for one `(source, external_id)` are presented once
  using the latest record.

Unknown LINE messages are ignored. If no history exists for today, the reply is
`今日尚無職缺異動。`.

## Local one-shot update binary

Use the separate `update-jobs` binary for a local one-shot update:

```bash
cargo run --bin update-jobs
```

It performs exactly one synchronization of all enabled sources, updates local `jobs.sqlite3`,
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
digest through LINE; when false, they only write local files and tracing logs.
Gmail delivery is controlled independently by the Gmail settings below. JD batch files are named
`jd-<run>-<chunk>.json` inside the date folder and are removed when their age
exceeds seven days. The normal `job-watcher` binary
enables the LINE bot and scheduler.

## Acquisition and state

104 remains a rendered Chromium/CDP source. LinkedIn is optional and uses the
Gmail API to read Job Alert messages, then fetches public LinkedIn pages without
login cookies or browser sessions. See
[docs/linkedin-source.md](docs/linkedin-source.md) for OAuth setup, alert
parsing, public-page mapping, and failure safety. Set `LINKEDIN_ENABLED=true`,
provide `GMAIL_OAUTH_CLIENT_FILE` and `GMAIL_OAUTH_TOKEN_FILE`, and configure
`LINKEDIN_GMAIL_QUERY` if the default search is not suitable.

During synchronization, LinkedIn IDs already present in SQLite with a complete
description are reused without another public-page HTTP request. Their
`last_seen_at` and `seen_count` are updated, and they are included as updated
jobs in the combined change history, Gmail summary, and `job-list.json`.
Stored IDs without a description are fetched again so their JD can be completed.
Only newly discovered or incomplete LinkedIn IDs need detail-page fetching.
If LinkedIn returns HTTP 401/403/429/999 or redirects to a login/access wall,
remaining detail requests stop for that synchronization. Unattempted jobs retain
their alert metadata with `MetadataOnly` and a deferred-request explanation;
cached complete JDs are still reused. This avoids continuing a blocked batch,
but does not remove LinkedIn's access restriction or persist a cooldown across
manual restarts. `Retry-After` is recorded when supplied.

Search cards provide summary fields. New and known jobs are opened in a second
normal Chromium tab and the complete visible detail text is extracted. A detail
failure is logged; a previous valid JD is preserved and empty failed content
never replaces it. The browser waits up to 30 seconds for rendered search cards
or a JD container with at least 40 characters instead of sleeping for a fixed
two seconds. Generic page-body text is not accepted as a JD. Missing JDs and
failed detail fetches are retried on the next synchronization, even when listing
fields have not changed. Incremental mode uses recent-first listing fields as a
pre-filter; `update-jobs-all` is available when every JD must be checked. See
[docs/104-source.md](docs/104-source.md).

Both sources expose `fetch_state` and an optional `fetch_error` in SQLite,
`job-list.json`, and daily change records. A failed 104 refresh keeps the old JD
but reports `FetchFailed`; a successful refresh clears the error. LinkedIn
errors include HTTP status and final URL when available, or the request error
chain. A successful HTTP response without a supported description reports
`ParseFailed`; redirects to login/access-wall URLs report `FetchFailed`.
Existing databases receive the nullable `fetch_error` column automatically.
This improves diagnosis; it does not make an access-restricted website available.

To capture one update's diagnostic log on the Pi:

```bash
RUST_LOG=job_watcher=debug cargo run --bin update-jobs 2>&1 | tee job-watcher-fetch.log
```

If 104 fails at `http://127.0.0.1:9222/json/version`, start Chromium first and
check that endpoint from the same host/container as the watcher. This failure
happens before visiting 104. `JOB_WATCHER_CDP_URL` can override the version
endpoint when the browser runs elsewhere; see [install.md](install.md).
LINE rejection errors include HTTP status, the response body (up to 2048
characters), and LINE's request ID, so token, recipient, quota, and other API
errors can be distinguished without logging request credentials.

SQLite remains the only current-state store. Its identity is `(source,
external_id)`, and existing databases are migrated in place. Deterministic
comparison reports New/Updated/Deleted/Unchanged; textual comparison ignores
line-ending and insignificant whitespace differences. Updated history includes
`changed_fields` and the latest complete JD.

## Change exports and Gmail

Each successful synchronization appends a run to `changes/YYYY-MM-DD.json`.
The JSON contains `date` and `runs`; each run has `generated_at`, `trigger`, and
`new`, `updated`, `deleted` arrays. New/updated records contain complete
normalized data and `description`; deleted records contain identity, summary,
URL, and deletion timestamp. The current day and previous six calendar days
are retained. Earlier runs on the same day are never overwritten.

When `GMAIL_SMTP_USERNAME`, `GMAIL_SMTP_APP_PASSWORD`, and
`JOB_WATCHER_EMAIL_TO` are configured,
each successful synchronization also sends a Gmail message with subject
`YYYY/MM/DD JD更新`. The body contains the `新增`, `更新`, and `刪除` counts,
followed by the title and URL of each changed job. The complete daily history
is attached as `YYYY-MM-DD.json`. The SMTP password must be a Gmail app
password. Gmail delivery failures are logged and do not roll back the
synchronization.

To verify SMTP delivery manually without running a synchronization:

```bash
job-watcher --test-email
```

This sends the production-formatted test message and does not access 104 or
modify SQLite or change history.

To send the latest saved change history without running a synchronization:

```bash
cargo run --bin send-latest-changes
```

This sends the latest run's counts, job titles, and URLs with the existing
daily JSON file attached. It does not modify SQLite or change history.

To inspect LinkedIn Job Alert URLs from Gmail without fetching LinkedIn pages
or modifying SQLite/history:

```bash
cargo run --bin search-linkedin-alerts
```

`JOB_WATCHER_JD_DIR` controls the local full-JD batch folder and defaults to
`changes`. The daemon uses the same date-folder and 10-JD batch layout as the
one-shot updater. No Google Drive or other cloud upload is performed.

Install Chromium, Rust, and CJK fonts as described in [install.md](install.md),
then start Chromium with remote debugging on `9222`.

## Verification

```bash
cargo fmt --check
cargo clippy --offline --locked --all-targets --all-features -- -D warnings
cargo test --offline --locked --all-features
```

The optional Chromium fixture test uses only local `data:` pages. With a test
Chromium CDP instance on port 9223, run:

```bash
cargo test --all-features chromium_waits_for_delayed_content_and_rejects_page_shell -- --ignored
```

Set `JOB_WATCHER_TEST_CDP_URL` to override its default
`http://127.0.0.1:9223/json/version`. It does not access 104 or LinkedIn.
