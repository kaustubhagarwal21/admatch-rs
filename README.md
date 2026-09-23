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

> **Work in progress.** The workspace, core API types and CI are in place; the
> matching engine, auction, HTTP API and benchmarks are being built. Every
> performance number will come from a recorded run; until then they are TBD.

## Development

Requires stable Rust (edition 2024).

```sh
make check   # fmt --check, clippy -D warnings, tests (same as CI)
make run     # start the server
```

## License

MIT, see [LICENSE](LICENSE).
