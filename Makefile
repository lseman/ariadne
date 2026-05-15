# Makefile for the Ariadne workspace

CARGO := cargo
CRATE := ariadne-cli
BINARY := target/release/ariadne

.PHONY: all help build release test fmt fmt-check clippy doc clean install run run-debug self-demo

all: release

help:
	@echo "Usage: make <target>"
	@echo "Available targets:"
	@echo "  all         Build release artifacts (default)"
	@echo "  build       Build workspace in debug mode"
	@echo "  release     Build workspace in release mode"
	@echo "  test        Run workspace tests"
	@echo "  fmt         Format all workspace Rust sources"
	@echo "  fmt-check   Check Rust formatting without changing files"
	@echo "  clippy      Run Clippy on the workspace"
	@echo "  doc         Build workspace documentation"
	@echo "  clean       Remove build artifacts"
	@echo "  install     Install the CLI binary from workspace"
	@echo "  run         Run the CLI in debug mode with optional arguments"
	@echo "  run-debug   Run the built debug binary with optional arguments"
	@echo "  self-demo   Execute the example self-demo script"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

test:
	$(CARGO) test

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

doc:
	$(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean

install:
	$(CARGO) install --path crates/ariadne-cli --force

run:
	$(CARGO) run -p $(CRATE) -- $(filter-out $@,$(MAKECMDGOALS))

run-debug:
	$(CARGO) run -p $(CRATE) --debug -- $(filter-out $@,$(MAKECMDGOALS))

self-demo:
	bash examples/self_demo.sh
