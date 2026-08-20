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

For a one-shot local update without the LINE bot or scheduler:

```bash
cargo run --bin update-jobs
```

This binary writes `jobs.sqlite3`, `job-list.json`, `job-list.html`, and local
`changes/YYYY-MM-DD.json`, plus only changed full-JD batch files under a date
folder such as `changes/08-16/`, with at most 10 JDs per file. Each completed
group of 10 JDs is written immediately while processing. Set
`JOB_WATCHER_LINE_BOT=true` to send the completion digest to LINE; otherwise it
only writes local files and tracing logs. Gmail delivery is configured
independently through the Gmail settings.
Use `cargo run --bin update-jobs-all` to refresh every JD batch.
Batch files use names such as `jd-20260816T1430120800-001.json`; files and
empty date folders older than seven days are removed. Set
`JOB_WATCHER_JD_DIR` to change the root folder. The daemon uses the same
date-folder and 10-JD batch layout as the one-shot updater.

Use `cargo run --release` for an optimized build. The process starts an Axum
health server on `http://127.0.0.1:3004/` by default and runs the watcher immediately,
then every day at 06:30 Asia/Taipei. The service also performs one
synchronization when it starts:

```bash
curl http://127.0.0.1:3004/
```

Configure the LINE webhook URL as `https://your-public-host/webhook` in the
LINE Developers console. The endpoint accepts `POST /webhook` JSON events and
returns HTTP 200.

Stop the service with `Ctrl-C`.

## Raspberry Pi configuration

### Build the Raspberry Pi binary

The Makefile's Raspberry Pi target is a direct Cargo target build:

```bash
rustup target add aarch64-unknown-linux-gnu
make build-rpi
```

The target runs:

```bash
cargo build --release --target aarch64-unknown-linux-gnu
```

When building from an x86 Linux machine, install an AArch64 linker first:

```bash
sudo apt install gcc-aarch64-linux-gnu
```

The binary is written to:

```text
target/aarch64-unknown-linux-gnu/release/job-watcher
```

Copy the binary to the Pi. The Pi still needs Chromium, its fonts, and the
configuration file below. A 32-bit Raspberry Pi OS installation needs a
different Rust target and linker; this Makefile target is currently for
64-bit Raspberry Pi OS.

For a system deployment, create the configuration directory and copy the
template outside the repository:

```bash
sudo install -d -m 750 /etc/job-watcher
sudo install -m 600 deploy/job-watcher.env.example /etc/job-watcher/job-watcher.env
sudo editor /etc/job-watcher/job-watcher.env
```

The service automatically loads `/etc/job-watcher/job-watcher.env` when that
file exists. It contains the HTTP port, LINE credentials, change directory, and
optional Gmail settings. The local `.env` file is used only when the system
configuration file does not exist.

Keep the file readable only by the account that runs the service because it
contains the LINE channel access token.

## Configure LINE notifications

LINE notifications are optional. To enable them:

```bash
cp .env.example .env
```

Edit `.env` and set:

```dotenv
LINE_CHANNEL_ACCESS_TOKEN=your-channel-access-token
LINE_USER_ID=your-recipient-user-id
```

Both values must be present together. The channel access token comes from the
LINE Messaging API channel, and the user ID identifies the recipient. Keep
`.env` private; it is ignored by Git. Without these values, the service still
prints `[New]`, `[Update]`, and `[Delete]` events locally.

## What to expect

Every check scans every result page so the current result set can be compared
with SQLite. Output includes:

- `[New]` for a new external job ID;
- `[Update]` when an existing job's tracked fields changed;
- `[Delete]` when a saved job is absent from the current search;
- no output for unchanged jobs.

The service writes these files in its current directory:

- `jobs.sqlite3` — persistent job database;
- `job-list.json` — latest extracted records;
- `job-list.html` — rendered first-page capture.

The database stores each job's `last_updated`, `work_site`, and
`annual_salary`. Existing databases receive these columns automatically on the
next service start.

When LINE is configured, startup and the scheduled 06:30 check send a daily
count digest after SQLite/history persistence. `更新JD` runs the same pipeline immediately;
`今日履歷` reads today's local history without contacting 104. When Gmail is
configured, the daily history is sent as a JSON attachment.

If Chromium is not listening on port `9222`, the watcher reports a connection
error. The project uses normal public-page rendering and does not bypass
CAPTCHA, Cloudflare challenges, authentication, or other access controls.
