.PHONY: build test lint fmt deny release clean

build:
	cargo build --workspace

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all
	cargo fmt --check --all

deny:
	cargo deny check

release:
	cargo build --release

clean:
	cargo clean
