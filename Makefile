.PHONY: build test lint fmt deny release clean regression

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

# Local regression: run the sharded-audit eval scenario against a live LLM.
# Credentials come from .env (git-ignored — copy .env.example). Costs real
# tokens; baseline-diff a previous report with:
#   make regression OUT="--baseline eval-results/eval-report.json"
regression: build
	. ./.env && ./target/debug/clausura eval --config eval/eval.yaml \
		--scenario sharded-security-audit --model "$${CLAUSURA_MODEL}" \
		--out-dir eval-results $(OUT)
