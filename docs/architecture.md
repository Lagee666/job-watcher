# Architecture

The preserved dependency flow is:

```text
104 Chromium/CDP ────────┐
                         │
LinkedIn Alert ─ Gmail API ─┴→ normalized Job → SQLite comparison
                                      → SQLite transaction
                                      → daily JSON export → Gmail attachment → LINE/Gmail
```

`jobs.sqlite3` is the authoritative latest state. `changes/YYYY-MM-DD.json`
is history/export only and is never used as current state.

## Runtime paths

Axum and the in-process scheduler share a `Service`. The scheduler waits for
startup and then the next `07:00 Asia/Taipei` instant. Webhook
handling recognizes `更新JD` and `今日履歷`. Browser, Gmail API, SQLite, filesystem,
SMTP, and LINE calls run on blocking threads rather than Tokio executor
threads.

`更新JD` uses a process-local mutex guard. If another cycle owns it, the caller
gets `目前正在更新 JD，請稍後再試。`. `今日履歷` does not acquire the guard or
contact 104.

## Synchronization pipeline

1. Acquire every enabled source: 104 uses Chromium/CDP; LinkedIn searches Gmail
   Job Alert messages and fetches public job pages over HTTP.
2. Reject incomplete or untrusted source snapshots so one provider failure
   cannot cause mass deletions. LinkedIn deletion comparison is source-scoped.
3. Normalize both sources into the same Job model and deduplicate LinkedIn
   results by `(source, external_id)`.
4. Compare `(source, external_id)` and normalized meaningful fields.
5. Commit all source upserts and safe deletions in the shared SQLite table.
6. Append one combined run to today's history JSON.
7. Retain seven calendar days locally.
8. Send the scheduled/manual digest. When Gmail is configured, the message uses
   `YYYY/MM/DD JD更新` as its subject, contains the three change counts plus
   each changed job's title and URL, and attaches that day's `YYYY-MM-DD.json`
   history. Downstream failures never roll back the committed SQLite state.

A failed history write is reported as an export failure. Gmail and LINE failures
are logged after persistence/export.

## One-shot binaries and local JD files

`update-jobs` runs one local incremental synchronization. It uses the
recent-first search result as a pre-filter, writes only changed/new full JDs,
and does not initialize Axum or scheduling. `JOB_WATCHER_LINE_BOT=true` enables
LINE completion delivery; when false, output is local files plus tracing logs.
Gmail delivery is configured independently. Full JDs are stored
under `JOB_WATCHER_JD_DIR` (default `changes/`) as timestamped JSON batches
containing at most 10 jobs per file. Batches are streamed to disk after each
group of 10 jobs under a date folder such as `08-16/`.

`update-jobs-all` runs one local full synchronization and refreshes every JD
batch. Both binaries retain local SQLite, `job-list.json`, `job-list.html`, and
daily change history. Local JD batch files and empty date folders older than
seven days are removed.

## Domain and comparison

The normalized job contains source identity, title, company, location, salary,
URL, appearance date, and complete description. 104 selectors and browser
details stay in `watcher.rs`. Gmail/MIME parsing and LinkedIn normalization stay
in `source/` and do not leak into the domain, repository, or
notification code. Both sources use the same `(source, external_id)` SQLite
identity, combined change history, Gmail attachment, and LINE reporting. Text
comparison normalizes line endings and insignificant whitespace.
Updated records include deterministic `changed_fields`.
