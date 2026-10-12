.PHONY: build test test-rust test-node test-python fmt lint verify test-services deny

# The tests' PostgreSQL (scripts/test-services.sh). 127.0.0.1, not localhost: localhost may
# resolve to ::1, where nothing listens.
PID_TEST_DATABASE_URL ?= postgres://postgres:test@127.0.0.1:25434/pid
export PID_TEST_DATABASE_URL

build:
	cargo build --workspace
	npm run build

test: test-rust test-node test-python

test-rust:
	cargo test --workspace

test-node:
	npm test

test-python:
	cd python && uv run pytest -q

fmt:
	cargo fmt --all
	npx prettier --write .
	cd python && uv run ruff format .

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	npm run lint
	npm run check
	cd python && uv run ruff check . && uv run ruff format --check .

verify: lint test

test-services:
	scripts/test-services.sh

deny:
	cargo deny --locked check
