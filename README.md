# Rust Job Watcher

Rust Job Watcher is a small Rust service that monitors public Rust-related job
listings on 104.com.tw. It renders the normal public search page in Chromium,
extracts the visible job cards, compares them with a local SQLite database, and
prints newly created or changed jobs.

## What runs

The executable contains two parts:

1. An Axum HTTP server listening on port `3004`.
2. A background watcher that performs the 104 synchronization.

The HTTP server currently provides a simple health response at `/`. The watcher
uses blocking browser, HTTP, and SQLite libraries, so it runs in a Tokio
blocking task and does not block Axum's async runtime.

## Check schedule

The watcher:

- runs once immediately when the service starts;
- runs every day at `07:00` and `17:00`;
- interprets those times in the machine's local timezone;
- stays running between checks.

```text
Start → HTTP server + initial check → sleep → 07:00 check → sleep → 17:00 check
```

## Change detection

The configured search is:

```text
https://www.104.com.tw/jobs/search/?jobsource=index_s&keyword=Rust&mode=s&order=16
```

Results are expected to be newest-first. The first run scans every result page.
Later runs stop at the first job ID already in SQLite, which avoids scanning
older listings.

For every listing encountered:

- `[CREATE]` means its job ID is not in the database;
- `[UPDATE]` means an existing listing's tracked fields changed;
- unchanged listings are silent;
- jobs missing from the current search are not reported as deleted.

Changed and new listings are upserted into SQLite. A job recreated by 104 with a
new external ID is therefore reported as `[CREATE]`.

## Quick start

### 1. Install prerequisites

On Debian, Ubuntu, or Raspberry Pi OS:

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config libssl-dev \
  chromium \
  fonts-noto-cjk fonts-noto-cjk-extra fonts-noto-color-emoji
```

Some distributions call the browser package `chromium-browser` instead of
`chromium`; install the name provided by that distribution.

Install Rust with [rustup](https://rustup.rs/) if it is not already installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Check the toolchain:

```bash
rustc --version
cargo --version
chromium --version
```

Rust libraries are declared in `Cargo.toml` and downloaded automatically by
Cargo. The main ones are Axum/Tokio for the service, `headless_chrome` for
rendering 104, `reqwest` for Chromium's local debugging connection, `rusqlite`
for SQLite, `serde` for job data, and `chrono` for scheduling.

### 2. Start Chromium

The watcher uses Chromium's normal remote-debugging interface to render the
public 104 page. Start it in a separate terminal:

```bash
chromium --remote-debugging-port=9222 \
  --user-data-dir=/tmp/job-watcher-chromium
```

Leave Chromium running while the watcher is running. If the executable is
called `chromium-browser`, replace the command name.

### 3. Run the service

From the repository directory, run the service in another terminal:

```bash
cargo run
```

For an optimized build, use `cargo run --release`.

Open `http://127.0.0.1:3004/` or run `curl http://127.0.0.1:3004/` to verify
that the Axum server is running. The process checks immediately, then remains
running for the 07:00 and 17:00 checks. Stop it with `Ctrl-C`. Full
installation notes are in [install.md](install.md).

## Files produced

The service writes these files in its current working directory:

- `jobs.sqlite3` — persistent job state and comparison source;
- `job-list.json` — records collected during the latest check;
- `job-list.html` — rendered HTML captured from the first search page.

The SQLite table uses `(source, external_id)` as its primary key. Existing jobs
are updated with the latest extracted values and `last_seen_at`.

## Repository map

```text
src/main.rs       Axum server and Tokio task orchestration
src/watcher.rs    104 browser extraction, scheduling, comparison, SQLite I/O
docs/architecture.md
                  Design and runtime data flow
docs/104-source.md
                  Verified 104 behavior, selectors, and operational assumptions
install.md        Local setup and Chromium startup instructions
AGENT.md          Instructions for contributors and coding agents
```

## 104 limitations

The watcher uses the publicly rendered page through normal browser rendering.
104's internal endpoints are undocumented and direct HTTP access may receive a
Cloudflare challenge. The project does not bypass CAPTCHA, authentication,
Cloudflare challenges, or other access controls. See
[docs/104-source.md](docs/104-source.md) before changing the acquisition
strategy.

## Development checks

Run these before submitting changes:

```bash
cargo fmt --check
cargo clippy --offline --locked --all-targets --all-features -- -D warnings
cargo test --offline --locked --all-features
```

The browser-backed integration run also requires Chromium listening on port
`9222`.

## Current scope

Implemented:

- 104 Rust search extraction through Chromium;
- virtualized-list scrolling and multi-page collection;
- SQLite persistence and full-record comparison;
- create/update logging;
- startup and twice-daily scheduling;
- Axum health endpoint.

Not yet implemented:

- notifications other than log output;
- a separate domain/repository abstraction;
- deletion history or deletion notifications;
- support for additional job platforms.

AI coding agents must read `AGENT.md` before modifying this repository.
