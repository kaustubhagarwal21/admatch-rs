# AdMatch

AdMatch is a low-latency, privacy-aware search-ads matching server written in
Rust (axum and tokio). For each search query it finds the campaigns whose
keywords match (exact, broad and negative match through an inverted index),
runs a quality-weighted generalised second-price auction on bid × relevance,
charges the winner against an atomically enforced daily budget, and applies
privacy thresholds: no personalised targeting of small audiences, and
k-anonymous reporting. The keyword match types and the 5,000-person targeting
threshold are modelled on publicly documented Apple Ads behaviour; the auction
and all implementation details are this project's own. All data is synthetic.

> **Work in progress.** The MVP is in place: the matching engine, the
> auction, in-memory daily budgets and the HTTP API (`/v1/match`, `/healthz`,
> `/metrics`) serving campaigns from a seed file. Postgres storage, the admin
> API, tap events, k-anonymous reports, load tests and shared (Redis) budgets
> are still being built. Every performance number comes from a recorded run;
> anything not measured yet is marked TBD.

## Quickstart

Needs only stable Rust (edition 2024): no Docker and no database.

```sh
make seed    # writes data/seed.json and data/requests.jsonl (fixed seed 42)
make run     # serves on 0.0.0.0:8080; settings come from env vars, see .env.example
```

The seeder prints how many campaigns it generated, how many it rejected and
why (for example `audience_too_small` for age targeting at or below the
5,000-person threshold), and how many request lines of each kind it wrote.

In a second terminal:

```sh
# 200 once the index is loaded, 503 before
curl -s localhost:8080/healthz

# Run one auction
curl -s localhost:8080/v1/match \
  -H 'content-type: application/json' \
  -d '{"query": "free photo editor", "country": "IN", "user_id": 918273645,
       "personalized": true, "age_bucket": "25-34"}'

# Same query with the ranking (top candidates, scores, exclusion reasons)
curl -s localhost:8080/v1/match \
  -H 'content-type: application/json' \
  -d '{"query": "free photo editor", "country": "IN", "user_id": 1, "debug": true}'

# Validation errors are JSON with a stable code, here 422 invalid_country
curl -s localhost:8080/v1/match \
  -H 'content-type: application/json' \
  -d '{"query": "photo", "country": "XX", "user_id": 1}'

# Prometheus metrics
curl -s localhost:8080/metrics
```

A match response has `impression_id` (a UUIDv7, or `null` when no ad is
shown), `ad` (campaign, keyword, match type, bid, price and relevance, or
`null`), `candidates` (campaigns that entered the auction) and `took_us`
(server-side time in microseconds). Errors always look like
`{"error": {"code": "...", "message": "..."}}`.

`data/requests.jsonl` holds complete request bodies for load testing.
Stop the server with ctrl-c; it finishes in-flight requests before exiting.

## Development

Requires stable Rust (edition 2024).

```sh
make check   # fmt --check, clippy -D warnings, tests (same as CI)
make run     # start the server
```

## License

MIT, see [LICENSE](LICENSE).
