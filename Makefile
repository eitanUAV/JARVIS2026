.PHONY: help build run dev test fmt lint check clean db-up db-down db-reset docker-build

help:
	@echo "JARVIS2026 — available commands:"
	@echo "  make db-up        Start PostgreSQL in Docker"
	@echo "  make run          Run the server (release)"
	@echo "  make dev          Run with auto-reload (needs cargo-watch)"
	@echo "  make test         Run the test suite"
	@echo "  make lint         Run clippy with warnings denied"
	@echo "  make fmt          Format the source"
	@echo "  make check        fmt --check + lint + test (what CI runs)"
	@echo "  make docker-build Build the production image"
	@echo "  make db-down      Stop PostgreSQL"
	@echo "  make db-reset     Drop and recreate the database volume"
	@echo "  make clean        Remove build output and uploaded files"

build:
	cargo build --release

run:
	cargo run --release

dev:
	cargo watch -x run

test:
	cargo test

fmt:
	cargo fmt

lint:
	cargo clippy --all-targets -- -D warnings

# Mirrors the CI pipeline so failures show up before pushing.
check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

docker-build:
	docker build -t jarvis2026:latest .

db-up:
	@echo "Starting PostgreSQL..."
	docker compose up -d postgres
	@sleep 5
	@echo "Database ready."

db-down:
	docker compose down

db-reset:
	docker compose down -v
	docker compose up -d postgres
	@sleep 5

clean:
	cargo clean
	rm -rf uploads/*
