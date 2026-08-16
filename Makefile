CARGO ?= cargo

.PHONY: help build test lint build-rpi clean

help:
	@printf '%s\n' \
		'make build       Build an optimized binary for the current machine' \
		'make test        Run the test suite' \
		'make lint        Run formatting and Clippy checks' \
		'make build-rpi   Build a Raspberry Pi aarch64 binary' \
		'make clean       Remove build artifacts'

build:
	$(CARGO) build --release

test:
	$(CARGO) test --all-features

lint:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings

build-rpi:
	$(CARGO) build --release --target aarch64-unknown-linux-gnu

clean:
	$(CARGO) clean

chromium:
	chromium --remote-debugging-port=9222 \
  	--user-data-dir=/tmp/job-watcher-chromium