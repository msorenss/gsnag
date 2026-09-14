ifneq ($(wildcard $(CURDIR)/.tools/cargo/bin/cargo),)
export CARGO_HOME := $(CURDIR)/.tools/cargo
export RUSTUP_HOME := $(CURDIR)/.tools/rustup
export PATH := $(CARGO_HOME)/bin:$(PATH)
endif
export CARGO_BUILD_JOBS ?= 3
ARGS ?= --help

.PHONY: build run test check fmt deb
build:
	cargo build --workspace
run:
	cargo run -p gsnag -- $(ARGS)
test:
	cargo test --workspace
check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
fmt:
	cargo fmt --all
deb:
	cargo build --locked --release -p gsnag
	python3 packaging/build-deb.py
