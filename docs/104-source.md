# 104 Job Source

## Investigation status

Investigation performed: 2026-08-09 (Asia/Taipei).

The target query is `keyword=Rust`. This document separates verified facts,
observations, assumptions, and unresolved questions. The endpoints described
below are website endpoints, not a documented public developer API contract.

## Executive findings

* No official, documented, public job-search API for job seekers was found.
  104 does advertise an API product for employers (resume transfer and job
  posting), which is a different product and does not document public search.
* The public search experience has used an internal JSON/XHR endpoint. Recent
  public technical documentation identifies it as:

~~~
GET https://www.104.com.tw/jobs/search/api/jobs
~~~

* A separate detail request has been observed for the full description:

~~~
GET https://www.104.com.tw/job/ajax/content/{job_id}
~~~

* A stable short job identifier is present in the job URL in documented
  response examples. The search/detail examples expose an `appearDate`, but
  no separately verified last-modified timestamp was found.
* Direct unauthenticated HTTP from this investigation was stopped by a
  Cloudflare managed challenge. No authentication or challenge bypass was
  attempted.

## 1. Official API

### Verified facts

104's official `104 招募管理 Pro` page describes `Resume API` and `JOB API`.
The page says these services transfer resumes to an internal system and
synchronize an employer's jobs with 104. It is not public job-search API
documentation and does not describe searching public listings.

Source: <https://ehr.104.com.tw/products/job-resume-api/>

### Conclusion

No official documented public job-search API was found. The internal website
endpoint must therefore be treated as an undocumented implementation detail,
not as a stable API.

## 2. Public search endpoint

### Verified facts from public request documentation/examples

Recent public documentation identifies the endpoint as:

~~~
GET https://www.104.com.tw/jobs/search/api/jobs
~~~

The endpoint is described as returning JSON and being used by the public job
search flow. The same documentation gives this minimal shape:

~~~
https://www.104.com.tw/jobs/search/api/jobs?jobsource=index_s&mode=s&page=1&pagesize=20
~~~

Source: <https://github-wiki-see.page/m/Li732375/JobE04_spider/wiki/Payload-%E6%89%80%E6%9C%89%E5%8F%83%E6%95%B8%E8%AA%AA%E6%98%8E>

An older, separately documented website flow used this endpoint instead:

~~~
GET https://www.104.com.tw/jobs/search/list
~~~

Source: <https://blog.jiatool.com/posts/job104_spider/>

This difference is evidence that the endpoint is subject to change; it is not
evidence that both endpoints remain interchangeable today.

### Query parameters

For the current `/api/jobs` endpoint, the following are documented/observed:

| Parameter | Meaning / use | Status |
|---|---|---|
| `keyword` | Search text; use `Rust` | verified in public request examples |
| `mode` | `s` is summary mode; `l` is list mode | verified in public documentation |
| `page` | Page number | verified in public documentation |
| `pagesize` | Requested page size; documented default is 20 | verified in public documentation |
| `jobsource` | Search origin, e.g. `index_s` or `joblist_search` | observed/documented, not required by a live test here |
| `searchJobs` | Value `1` appears in examples | observed/documented; meaning unresolved |
| `order` | Sort order | observed in public examples |
| `asc` | Sort direction | observed in public examples |
| `area`, `jobcat`, `label`, and filters | Optional search filters | documented as optional filters |

A conservative Rust query should begin with only the parameters required by
the public search URL and the endpoint's examples, for example:

~~~
keyword=Rust&mode=s&page=1&pagesize=20
~~~

Whether `jobsource` or `searchJobs` is required by the current production
endpoint is unresolved because the live JSON request was challenged.

## 3. Pagination

### Verified facts from public examples

Pagination uses a numeric `page` parameter. Public documentation says the
default page is the first page and that subsequent pages require increasing
`page`. It documents a website limit of no more than 150 pages and a default
`pagesize` of 20. It also says the displayed count may be two greater than
`pagesize`.

The older `/jobs/search/list` response example uses:

~~~
data.totalCount
data.totalPage
data.list
~~~

and stops when `page == totalPage` or `totalPage == 0`.

### Unresolved for the current endpoint

The current `/api/jobs` response's exact pagination fields, effective maximum
page size, whether the final page can be short, and duplicate behavior across
pages were not verified from a current live JSON response. An implementation
must use a defensive maximum page count and de-duplicate by job ID.

## 4. Response schema relevant to the domain

### Search-result fields documented in public examples

The older search JSON examples and accompanying code use these fields:

| Domain field | Search response field / derivation | Status |
|---|---|---|
| `external_id` | extract the ID from `link.job` URL | verified in public example code |
| `title` | `jobName` | verified in public example code |
| `company` | `custName` | verified in public example code |
| `location` | `jobAddrNoDesc` and/or `jobAddress` | verified in public example code |
| `salary` | `salaryDesc` (numeric `salaryLow`/`salaryHigh` also shown) | verified in public example code |
| `url` | `link.job`, usually protocol-relative and requiring `https:` | verified in public example code |
| `published_at` | `appearDate` is labelled as the listing/update date in the example | verified as a field; semantic mapping needs care |
| description | `descSnippet` is a search snippet only | verified in public example code |

The documented detail JSON has a top-level `data` object containing (among
others) `header`, `jobDetail`, `condition`, `welfare`, and `contact`.
Relevant detail fields include:

~~~
data.header.jobName
data.header.custName
data.header.appearDate
data.jobDetail.jobDescription
data.jobDetail.salary
data.jobDetail.salaryMin
data.jobDetail.salaryMax
~~~

The detail fixture also contains `data.closeDate`, but no independently
verified `updatedAt`, `lastModified`, or equivalent field was found.

Sources for the search/detail examples:

* <https://blog.jiatool.com/posts/job104_spider/>
* <https://github.com/it-jia/job104_spider/blob/main/job104_spider.py>
* <https://github.com/it-jia/job104_spider/blob/main/104_job.json>

These examples are historical and must be treated as schema evidence, not a
guarantee that every field still exists.

## 5. Full description request

### Verified facts from public request examples

The full description is loaded separately by a GET request:

~~~
GET https://www.104.com.tw/job/ajax/content/{job_id}
Referer: https://www.104.com.tw/job/{job_id}
~~~

The response examples are JSON, with the full text at:

~~~
data.jobDetail.jobDescription
~~~

Therefore, search results alone should be considered insufficient for the
domain's required `description` field. Fetching details is a second request
per selected listing.

## 6. Identity and timestamps

### Verified facts / observations

* Search examples derive a job ID from `link.job`; the URL form is
  `https://www.104.com.tw/job/{job_id}`.
* The detail endpoint uses that same short ID in its path.
* `appearDate` is available in both the documented search transformation and
  detail fixture. It is presented as an appearance/update date in those
  examples.

### Unresolved

It is not verified that the short ID remains unchanged when an employer edits
or republishes a posting. It is also not verified that `appearDate` is a true
last-modified timestamp rather than a publication, refresh, or display date.
No stable platform update timestamp suitable for `platform_updated_at` was
confirmed. Change detection should therefore initially rely on normalized
content hashing, with `appearDate` retained only if its semantics are
accepted as approximate.

## 7. Authentication, cookies, and headers

### Direct observation during this investigation

A read-only request to:

~~~
https://www.104.com.tw/jobs/search/api/jobs?keyword=Rust&mode=s&page=1&pagesize=20
~~~

returned `403 Forbidden` with headers identifying a Cloudflare managed
challenge (`Cf-Mitigated: challenge`) and a response instructing the client to
enable JavaScript and cookies. The response set a `__cf_bm` cookie. No browser
automation, CAPTCHA solving, cookie replay, or other bypass was attempted.

### Public examples

Public request examples report that a browser-like `User-Agent` and a matching
`Referer` are useful/required for the JSON requests. The detail example uses a
job-page referer; the search example uses the search-page referer.

No login credentials are shown in those public examples. This does not prove
that no session cookie or edge clearance cookie can ever be required.

## 8. Restrictions and low-frequency suitability

### Observations

* The public search page is accessible to search engines and visibly exposes
  listing summaries for `Rust`.
* Direct non-browser HTTP was challenged during this investigation.
* The search and detail endpoints are undocumented website internals.
* Public educational examples advise low-volume use and include multi-second
  delays between pages; this is guidance from those examples, not an official
  104 limit.

### Assumptions for this project

The planned two synchronization cycles per day are low frequency. The source
should make one bounded search request per page, avoid parallelism, fetch detail
only for listings needed by the domain, use a clear User-Agent and Referer, and
back off on `403`, `429`, or other edge errors. It must fail visibly rather
than attempt to evade a challenge.

### Unresolved questions

* The applicable 104 terms/robots policy for this exact automated use was not
  established from an authoritative 104 policy page during this investigation.
* Current request quotas, rate limits, and whether the challenge is persistent
  for a Raspberry Pi IP are unknown.
* The current JSON schema and current detail-header requirements need a
  permitted browser/manual capture or a future successful direct request.

## Direct HTTP validation — 2026-08-09

### Method

Three low-frequency, read-only requests were made with `curl` as the standard
HTTP client. Each request used only a normal browser-like `User-Agent` and a
matching `Referer`; no login, challenge interaction, CAPTCHA handling, cookie
replay, or browser automation was used.

Search request, repeated twice:

~~~
GET https://www.104.com.tw/jobs/search/api/jobs?keyword=Rust&mode=s&page=1&pagesize=20
User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36
Referer: https://www.104.com.tw/jobs/search/?keyword=Rust
~~~

Detail request:

~~~
GET https://www.104.com.tw/job/ajax/content/71gqf
User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36
Referer: https://www.104.com.tw/job/71gqf
~~~

### Verified results

| Request | HTTP status | Content-Type | Body size | JSON parsing | Result |
|---|---:|---|---:|---|---|
| Search, attempt 1 | 403 | `text/html; charset=UTF-8` | 5898 bytes | failed | Cloudflare challenge HTML |
| Search, attempt 2 | 403 | `text/html; charset=UTF-8` | 5898 bytes | failed | Cloudflare challenge HTML |
| Detail | 403 | `text/html; charset=UTF-8` | 5663 bytes | failed | Cloudflare challenge HTML |

All three responses included `Cf-Mitigated: challenge`, `Server: cloudflare`,
and an HTML body beginning with `<!DOCTYPE html>` and a `Just a moment...`
title. No `429` response or valid JSON was observed.

The two search attempts were deterministic in status, content type, body
size, and challenge classification. Their body hashes differed because the
challenge response contains per-response challenge values. The detail request
was challenged in the same way.

Each response included a `Set-Cookie` header for a `__cf_bm` cookie and the
challenge HTML instructed the client to enable JavaScript and cookies. This
verifies that cookies are involved in the challenge response. It does not
verify that obtaining or replaying that cookie would permit access, and no
such retry was attempted.

### Production conclusion

The provisional JSON search and detail endpoints are not reliably retrievable
from an ordinary non-browser HTTP client in the tested environment, even with
normal browser-like headers. This behavior is an access restriction, not a
JSON parsing problem.

The public HTML page is visible to search engines, but direct access from this
client was not reattempted as an HTML fallback in this validation. A normal
browser may render it, but making that the production source would require a
separate policy/terms decision and browser-based acquisition. Manually
captured, sanitized search/detail fixtures remain suitable for development and
parser tests only.

## Recommended production acquisition strategy

**Abandon automated 104 retrieval for production unless 104 provides an
official access arrangement or explicitly permits a supported integration.**

This is the single recommended strategy because both provisional endpoints
returned deterministic Cloudflare challenge HTML under normal HTTP access, and
the project explicitly forbids bypassing anti-bot protections. Use manually
captured fixtures to develop and test the 104 DTO/parser in the meantime; do
not implement the production `JobSource` against these endpoints.

## Playwright normal-rendering validation — 2026-08-09

The exact public URL requested for validation was opened in headless Chromium
using Playwright:

~~~
https://www.104.com.tw/jobs/search/?keyword=Rust&mode=s
~~~

The top-level document returned `HTTP 200` and `Content-Type: text/html`.
Playwright rendered the page normally without solving or interacting with any
challenge. However, the visible page reported `共 0 筆` and displayed no job
cards.

The browser's ordinary page requests showed why the list was empty:

| Request | HTTP status | Content-Type | Observation |
|---|---:|---|---|
| `/jobs/search/ajax/cards?keyword=Rust&mode=s&order=15` | 403 | `text/html; charset=UTF-8` | challenge HTML |
| `/jobs/search/api/recommend-job-filters` | 403 | `text/html; charset=UTF-8` | challenge HTML |
| `/jobs/search/api/jobs?keyword=Rust&mode=s&order=15&pagesize=20` | 403 | `text/html; charset=UTF-8` | challenge HTML |
| `/jobs/search/api/jobs?ro=1` | 403 | `text/html; charset=UTF-8` | challenge HTML |
| `/jobs/main/ajax/KeywordSuggest/mixSearch?scope=com&count=5&kw=Rust` | 403 | `text/html; charset=UTF-8` | challenge HTML |

Static CSS and JavaScript assets loaded with `200`, but they did not produce a
job list because the data requests were blocked. The browser console reported
failed resource loads for the `403` responses. This identifies
`/jobs/search/ajax/cards` as another current internal data endpoint, but does
not establish a usable production contract.

### Updated conclusion

Normal browser rendering does not provide a reliable acquisition path in the
tested environment: the public shell is reachable, while the data requests
needed to populate the job list are challenged. No job list was obtained from
the live page. The prior recommendation to abandon automated 104 retrieval
unless 104 provides an official supported access arrangement remains
unchanged.

## Local rendered-card extraction

The current development watcher uses normal browser rendering through an
already running Chromium CDP session and this search URL:

~~~
https://www.104.com.tw/jobs/search/?jobsource=index_s&keyword=Rust&mode=s&order=16
~~~

It reads the rendered pagination links and extracts rendered cards matching:

~~~
div[data-job-no]
~~~

The fields are read from the rendered card using these selectors/attributes:

| Output field | Rendered source |
|---|---|
| `external_id` | `data-job-no` |
| `title` | `.info-name` |
| `company` | `.info-company > a` |
| `location` | `[data-gtm-joblist^="職缺-地區"]` |
| `salary` | `[data-gtm-joblist^="職缺-薪資"]` |
| `url` | `a.info-job`, unwrapped from the `r.104.com.tw` redirect URL when possible |
| `published_at` | `.job-mobile__date` |
| `description` | `.info-content` when present; summary cards may have no description |

Because the page uses `vue-recycle-scroller`, extraction scrolls the list,
collects currently rendered cards, and de-duplicates by `data-job-no`. The first
run visits every result page. Later runs compare each listing with the full
record stored in SQLite, print `[CREATE]` for new listings and `[UPDATE]` when
any tracked field differs, and stop at the first known listing because
`order=16` is configured as newest-first. Listings absent from the current
result are not printed as deletions. The ordering assumption remains an
operational assumption, not a guarantee from a documented API.

The watcher writes the raw first-page rendering to `job-list.html` and the
newly extracted summary records to `job-list.json`. This is a development
capture path and does not change the warning that the underlying 104 endpoints
are protected and undocumented.
