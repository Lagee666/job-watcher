# LinkedIn Job Alert Source

## Investigation status

LinkedIn discovery uses Gmail Job Alert messages through Google's Gmail API.
The source does not use LinkedIn login, cookies, Chromium, Playwright,
Selenium, or authenticated LinkedIn endpoints.

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
`text/html` part. It extracts public LinkedIn job links and derives identity
from the numeric segment after `/jobs/view/`:

```text
https://www.linkedin.com/jobs/view/4454432978/?refId=x&trackingId=y
→ source=linkedin, external_id=4454432978
→ https://www.linkedin.com/jobs/view/4454432978/
```

Tracking parameters never participate in identity. Multiple messages, alerts,
or tracking URLs for the same ID become one job in a synchronization.

Processed Gmail message IDs are stored in
`LINKEDIN_PROCESSED_MESSAGES_FILE`. The source does not rely on read/unread
state, and records IDs only after MIME extraction and public-page processing
complete. A crash before that point allows the message to be retried.

For a read-only inspection of all matching alert messages, run
`cargo run --bin search-linkedin-alerts`. It prints normalized metadata,
canonical URLs, and LinkedIn IDs without fetching public pages or modifying
SQLite, change history, or the processed-message state.

## Public LinkedIn page mapping

The normalized public URL is fetched with a conservative sequential HTTP
client. No authentication is sent. The parser first checks `JobPosting`
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

## Failure and deletion safety

HTTP errors, 401/403 login walls, 429 rate limits, removed jobs, malformed
HTML, missing descriptions, and parser changes are recorded as per-job fetch
failures. Requests are sequential with a short delay; there is no aggressive
crawler or tight retry loop.

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
