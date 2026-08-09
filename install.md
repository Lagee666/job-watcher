# Installation and Running

## Prerequisites

On Debian, Ubuntu, or Raspberry Pi OS, install the native build tools,
Chromium, and fonts needed for Chinese job titles:

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config libssl-dev \
  chromium \
  fonts-noto-cjk fonts-noto-cjk-extra fonts-noto-color-emoji
```

Some distributions use `chromium-browser` instead of `chromium`:

```bash
sudo apt install -y chromium-browser
```

Install Rust with [rustup](https://rustup.rs/) if necessary:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Verify the required commands:

```bash
rustc --version
cargo --version
chromium --version
```

## Run locally

From the repository directory, start Chromium in one terminal:

```bash
chromium --remote-debugging-port=9222 \
  --user-data-dir=/tmp/job-watcher-chromium
```

If the executable is named `chromium-browser`, use that name instead. Leave
Chromium running while the watcher is running.

In a second terminal, start the service:

```bash
cargo run
```

Use `cargo run --release` for an optimized build. The process starts an Axum
health server on `http://127.0.0.1:3004/` and runs the watcher immediately,
then every day at 07:00 and 17:00 in the machine's local timezone:

```bash
curl http://127.0.0.1:3004/
```

Stop the service with `Ctrl-C`.

## What to expect

The first check scans every result page. Later checks stop at the first known
job because the 104 search is configured newest-first. Output includes:

- `[CREATE]` for a new external job ID;
- `[UPDATE]` when an existing job's tracked fields changed;
- no output for unchanged jobs or jobs absent from the current search.

The service writes these files in its current directory:

- `jobs.sqlite3` — persistent job database;
- `job-list.json` — latest extracted records;
- `job-list.html` — rendered first-page capture.

If Chromium is not listening on port `9222`, the watcher reports a connection
error. The project uses normal public-page rendering and does not bypass
CAPTCHA, Cloudflare challenges, authentication, or other access controls.
