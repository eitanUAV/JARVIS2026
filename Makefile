.PHONY: help build run dev clean db-up db-down db-reset test format

help:
	@echo "JARVIS2026 - Available Commands:"
	@echo "  make build      - Build release version"
	@echo "  make run        - Run the server"
	@echo "  make dev        - Run with auto-reload"
	@echo "  make db-up      - Start PostgreSQL database"
	@echo "  make db-down    - Stop PostgreSQL database"
	@echo "  make db-reset   - Reset database"

build:
	cargo build --release

run:
	cargo run --release

dev:
	cargo watch -x run

db-up:
	@echo "🐘 Starting PostgreSQL..."
	docker-compose up -d postgres
	@sleep 5
	@echo "✅ Database ready!"

db-down:
	docker-compose down

db-reset:
	docker-compose down -v
	docker-compose up -d postgres
	@sleep 5

clean:
	cargo clean
	rm -rf uploads/*

test:
	cargo test

format:
	cargo fmt
