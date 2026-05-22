.PHONY: build test lint fmt release clean

build:
	cargo build --workspace

test:
	cargo test --workspace

lint:
	cargo clippy --workspace -- -D warnings

fmt:
	cargo fmt --all
	cargo fmt --check --all

release:
	cargo build --release

clean:
	cargo clean
