# Developer shortcuts. Each target is a thin wrapper around one command, so
# `make <target>` and the command it runs are easy to compare.
#
# `make check` is the full local gate and matches what CI runs.

COMPOSE := docker compose -f deploy/docker-compose.yml

.PHONY: help fmt fmt-check lint test check run seed bench up down load

help:
	@echo "Targets: fmt fmt-check lint test check run seed bench up down load"

# Rewrite source files into the standard rustfmt style.
fmt:
	cargo fmt --all

# Fail (without rewriting) if anything is not formatted; used by CI.
fmt-check:
	cargo fmt --all --check

# Clippy with every warning treated as an error.
lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --workspace

# Everything CI checks, in the same order.
check: fmt-check lint test

run:
	cargo run -p admatch-server

seed:
	@echo "seed: available from M3 (seed data generator)"

bench:
	@echo "bench: available from M1 (criterion benches)"

# Start and stop the local Postgres container.
up:
	$(COMPOSE) up -d

down:
	$(COMPOSE) down

load:
	@echo "load: available from M6 (Go load generator)"
