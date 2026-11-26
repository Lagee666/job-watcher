# Greenhouse company JD enrichment

Acquisition uses the documented public Job Board detail API, without
authentication. It enriches a LinkedIn alert only when that email card provides
a direct `https://boards.greenhouse.io/{board}/jobs/{id}` or
`https://job-boards.greenhouse.io/{board}/jobs/{id}` link. It does not discover or
paginate company inventories.

The request is `GET https://boards-api.greenhouse.io/v1/boards/{board}/jobs/{id}`.
The parser requires the returned numeric `id`, `title`, `company_name`,
`location.name` when supplied, and meaningful `content`. `first_published` is
retained when available. Encoded HTML content becomes plain text.

IDs must match the link; titles/employers/known locations must match the email
metadata. No fuzzy matching is performed. Successful enrichment preserves the
LinkedIn identity and records the company page as `description_url`.

The shared enrichment limit is 20 sequential requests per run, with one second
between requests, a 20-second timeout and 2 MiB body limit. Errors, redirects,
schema changes, and mismatches retain metadata instead of a complete JD.
Fixtures are synthetic/sanitized; live company access is not required in tests.

Reference: <https://docs.greenhouse.io/job-board.html>
