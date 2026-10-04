.PHONY: build check test fmt lint run-core run-analytics ui-dev ui-build up down

build:
	cargo build --workspace
	cd ui && npm run build

check:
	cargo check --workspace
	cd ui && npm run check

test:
	cargo test --workspace

fmt:
	cargo fmt --all
	cd ui && npm run format

lint:
	cargo clippy --workspace -- -D warnings
	cd ui && npm run lint

run-core:
	cargo run -p pacod-gw-core

run-analytics:
	cargo run -p pacod-gw-analytics

ui-dev:
	cd ui && npm run dev

ui-build:
	cd ui && npm run build

up:
	docker compose -f deploy/docker-compose.yml up -d --build

down:
	docker compose -f deploy/docker-compose.yml down
