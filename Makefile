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

# Start the HTTP server (release build; settings come from the environment,
# see .env.example). It serves data/seed.json, so run `make seed` first.
run:
	cargo run --release -p admatch-server --bin admatch-server

# Generate data/seed.json and data/requests.jsonl. The fixed seed makes the
# output identical on every machine; override with `make seed SEED=7`.
SEED ?= 42
seed:
	cargo run --release -p admatch-server --bin seed -- --seed $(SEED)

# Criterion microbenchmarks for keyword matching and the auction. Results
# are written to target/criterion; see docs/BENCHMARKS.md for recorded runs.
# The bench targets are named so criterion flags can be appended, e.g.
# `make bench BENCH_ARGS="--warm-up-time 2 --measurement-time 5"` (the
# library's own test harness would reject them).
BENCH_ARGS ?=
bench:
	cargo bench -p admatch-core --bench match --bench auction -- $(BENCH_ARGS)

# Start and stop the local Postgres container.
up:
	$(COMPOSE) up -d

down:
	$(COMPOSE) down

load:
	@echo "load: available from M6 (Go load generator)"
