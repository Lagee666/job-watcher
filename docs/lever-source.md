# Lever company JD enrichment

Acquisition uses the documented public Postings detail API without login.
It accepts only direct job links associated with a single LinkedIn email card:
`https://jobs.lever.co/{site}/{id}` or the `jobs.eu.lever.co` equivalent.
There is no company inventory discovery or pagination.

The corresponding request is
`GET https://api.lever.co/v0/postings/{site}/{id}?mode=json`, using
`api.eu.lever.co` for EU links. Returned `id` and `text` must match; supplied
`categories.location` must agree with the email. The API does not independently
identify the employer: company association comes from the direct link in that
same email card, not from a guessed board name or a search result.

The JD includes `descriptionPlain` (HTML fallback), every requirements `lists`
section, and `additionalPlain` (HTML fallback). Plain-text C++/generic syntax is
preserved. LinkedIn identity stays unchanged; `description_url` identifies the
company job page. Salary and posting date are not inferred.

Shared limits are 20 sequential requests per run, one-second spacing, 20-second
timeout, 2 MiB responses, and no redirects. Failures/mismatches keep metadata.
Parser fixtures are synthetic and do not assert live platform availability.

Reference: <https://github.com/lever/postings-api>
