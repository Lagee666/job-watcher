# LinkedIn Job Alert Source

## Investigation status

LinkedIn discovery uses Gmail Job Alert messages through Google's Gmail API.
The source does not use LinkedIn login, cookies, Chromium, Playwright,
Selenium, or authenticated LinkedIn endpoints. Public-page fetching is disabled
by default; enable it explicitly with `LINKEDIN_FETCH_PUBLIC_DETAILS=true`.

The Gmail search is configurable and defaults to:

```text
from:(linkedin.com) newer_than:30d
```

The sender and exact alert layout are intentionally not treated as fixed
contracts. `LINKEDIN_GMAIL_QUERY` can narrow the search after inspecting the
account's actual messages.

## OAuth setup

Set:

```dotenv
LINKEDIN_ENABLED=true
GMAIL_OAUTH_CLIENT_FILE=/etc/job-watcher/google-oauth-client.json
GMAIL_OAUTH_TOKEN_FILE=/var/lib/job-watcher/gmail-oauth-token.json
LINKEDIN_PROCESSED_MESSAGES_FILE=/var/lib/job-watcher/linkedin-processed-gmail-messages.json
```

The client file is a Google OAuth installed/web client JSON file. The token
file contains the refresh token and is outside Git. Run:

```bash
cargo run --bin gmail-auth
```

once on the Raspberry Pi (or another machine that can reach the configured
loopback callback), open the printed Google consent URL, approve Gmail
readonly access, and leave the process running until it saves the token.
Subsequent runs refresh the access token without interactive login.

The implementation requests only:

```text
https://www.googleapis.com/auth/gmail.readonly
```

## Alert extraction

For each matching Gmail message, the source reads the MIME tree and selects a
`text/html` part and retains its subject. HTML is parsed as a DOM, with no
browser or JavaScript execution. It extracts anchors on real `linkedin.com`
hosts, including regional hosts, `/comm/jobs/view/`, and slugged URLs ending in
a numeric job ID. Tracking parameters never participate in identity:

```text
https://www.linkedin.com/jobs/view/4454432978/?refId=x&trackingId=y
→ source=linkedin, external_id=4454432978
→ https://www.linkedin.com/jobs/view/4454432978/
```

Tracking parameters never participate in identity. Multiple messages, alerts,
or tracking URLs for the same ID become one job in a synchronization.

Each link is associated with a surrounding card containing only that LinkedIn
job ID, so company/location/career links are not copied from neighboring jobs.
Metadata comes from explicit company/location attributes, company links, and
supported adjacent text layouts such as `Company · Location`. Generic buttons
do not replace meaningful titles. Duplicate alerts fill missing metadata
instead of keeping the first incomplete record forever. A subject fallback is
used only for a single job or an exactly matching primary title, never all
recommendations. Absent information remains unknown. These are layout
heuristics, not a guarantee that every LinkedIn email template is supported.

## Company JD enrichment

The default flow uses email metadata first. A single company-job link directly
associated with that email card may enrich its description:

- Greenhouse job links use the public Job Board detail API; see
  [greenhouse-source.md](greenhouse-source.md).
- Lever job links use the public Postings detail API; see
  [lever-source.md](lever-source.md).
- Other HTTPS career hosts must be explicitly listed in
  `JOB_WATCHER_CAREER_HOSTS`. Their page must contain exactly one matching
  `JobPosting` JSON-LD record, including title and employer. No generic body
  text or JavaScript/browser fallback is used.

Titles must match after case/whitespace normalization (C++, levels, and other
meaningful punctuation are preserved). Company names are checked when the
provider supplies them, and supplied locations must agree. API job IDs must
match the email's company link. Ambiguous links or mismatched records are not
accepted. There is no automatic company-site search or fuzzy title matching.
Google, Microsoft, or other postings with only a LinkedIn link stay metadata
only unless a supported, directly associated company link is available.

Requests are sequential, separated by one second, capped at 20 per run, bounded
to 20 seconds/2 MiB per response, and never follow redirects. Only fixed ATS
API hosts or explicitly configured company hosts are fetched. Company failures
preserve email data and report `fetch_error`; successful JDs include their
company job URL in `description_url`. LinkedIn identity and original URL remain
unchanged. Previously complete JDs are reused without network access.

Processed Gmail message IDs are stored in
`LINKEDIN_PROCESSED_MESSAGES_FILE`. The source does not rely on read/unread
state, and records IDs only after MIME extraction and public-page processing
complete. A crash before that point allows the message to be retried.

For a read-only inspection of all matching alert messages, run
`cargo run --bin search-linkedin-alerts`. It prints normalized metadata,
canonical URLs, and LinkedIn IDs without fetching public pages or modifying
SQLite, change history, or the processed-message state.

## Public LinkedIn page mapping

When `LINKEDIN_FETCH_PUBLIC_DETAILS=true`, incomplete LinkedIn jobs may be
fetched from their normalized public URL with conservative
sequential HTTP requests, waiting ten seconds between links. A job ID already
stored in SQLite with a non-empty description reuses its stored normalized
record and is not fetched again; its seen-tracking values are updated by the
watcher. Stored records without a description are fetched again for enrichment.
No authentication is sent.
The parser first checks `JobPosting`
JSON-LD, then stable metadata/description structures such as `og:title`,
`show-more-less-html__markup`, `description__text`, and
`job-description`.

The mapping is:

| Normalized field | Public page source |
| --- | --- |
| `source` | constant `linkedin` |
| `external_id` | numeric LinkedIn job ID |
| `title` | JSON-LD `title`, then alert title |
| `company` | JSON-LD `hiringOrganization.name`, then alert metadata |
| `location` | alert metadata or JSON-LD job location |
| `description` | JSON-LD description or visible description markup |
| `url` | canonical LinkedIn job URL |
| `published_at` | JSON-LD `datePosted` when present |
| `salary` | currently unavailable from the alert/page parser, so `null` |

The complete JD is preserved when a sufficiently complete description is
available. Fetch state is explicit: `Complete`, `MetadataOnly`, `FetchFailed`,
or `ParseFailed`. Metadata-only records retain source, ID, URL, title, and
company so a failed public page does not crash the entire synchronization or
erase a previously complete description.

Failure diagnostics are persisted as optional `fetch_error` text in SQLite and
JSON exports, rather than appearing only in logs. Non-success responses include
HTTP status and final URL (404 retains the `Removed` state); request failures
include their error chain and a timeout/connection/redirect/request category.
HTTP 200 redirects to `/authwall`, `/login`, `/uas/login`, or `/checkpoint`
are treated as `FetchFailed`. A successful response without a supported description is
`ParseFailed`, with final URL and body length recorded. Successful extraction
has no `fetch_error`. No extra retries, browser fallback, credentials, or
access-control bypasses are introduced.

## Failure and deletion safety

HTTP errors, 401/403 login walls, 429 rate limits, removed jobs, malformed
HTML, missing descriptions, and parser changes are recorded as per-job fetch
failures. Requests are sequential with a short delay; there is no aggressive
crawler or tight retry loop.

HTTP 401, 403, 429, and nonstandard 999 responses, plus successful redirects to
a login/access wall, stop all further public-page requests for that
synchronization. The triggering job retains `FetchFailed`; remaining jobs that
need enrichment retain their alert metadata with `MetadataOnly` and an explicit
deferred-request reason. Complete cached records continue to be reused, and no
discovered identities are discarded. HTTP 404 and ordinary per-job parse
failures do not stop unrelated jobs. `Retry-After` is recorded in the diagnostic
when supplied. This guard is per synchronization; it does not persist cooldown
state across restarts or guarantee that a later run will have access. Avoid
repeated manual runs while LinkedIn is rejecting requests.

Alert discovery is additive evidence, not a complete LinkedIn inventory.
LinkedIn snapshots therefore never delete missing rows. Existing LinkedIn
rows are preserved when Gmail fails, OAuth expires, a message is malformed, or
a public job page cannot be fetched. The existing 104 source retains its
current source-scoped deletion behavior, and 104/LinkedIn records remain
independent under `(source, external_id)`.

## Known limitations

Gmail alert HTML can change, and a public LinkedIn page may require login or be
removed. Alert metadata is only as rich as the received email. The parser does
not claim to expose every optional LinkedIn field such as employment type,
seniority, function, or industry. Gmail API list pagination is currently
bounded to the first 100 matching messages per run; the configurable query and
processed-message state are intended to keep that window focused.
