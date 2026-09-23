# Benchmarks

Every number on this page comes from a run that was actually executed, with
the command and machine recorded next to it. Anything not measured yet is
marked TBD.

## Machine

| | |
|---|---|
| CPU | 13th Gen Intel Core i9-13900HX (Windows reports 24 cores and 32 logical processors; `lscpu` inside WSL reports 32 CPUs) |
| RAM | 15.7 GB on the Windows host; the WSL2 VM sees 7.6 GiB (`MemTotal: 7987020 kB`) |
| OS | Windows 11 Home host, WSL2 Ubuntu 24.04.3 LTS, kernel `5.15.167.4-microsoft-standard-WSL2` |
| Rust | `rustc 1.98.1 (48a229cea 2026-09-01)`, criterion 0.8.2, `bench` profile (optimised) |
| Conditions | Nothing else was building or running (checked with `ps`). CPU frequency scaling and power settings were left at their defaults, so expect some run-to-run variation. |
| Date | 2026-09-23 |

## Criterion microbenchmarks

Command (run from the repository root inside WSL):

```sh
cargo bench -p admatch-core --bench match --bench auction -- --warm-up-time 2 --measurement-time 5
```

The two bench targets are named explicitly. With only `-p admatch-core`, the
library's built-in test harness also runs as a bench target and rejects
criterion's `--warm-up-time` flag.

Each benchmark collected 100 samples after a 2-second warm-up. The medians
and their 95% confidence intervals below come from criterion's
`target/criterion/<group>/<id>/new/estimates.json`.

### `match`: keyword index lookup

One iteration runs `Snapshot::matches` on each of the 8 queries in a fixed
query set (exact and broad hits, near misses and one no-match query). That
covers tokenising, index lookup, negative filtering and choosing the best
keyword per campaign. It does not include the auction. The corpus comes from a
deterministic generator: 10 keywords per campaign, 1 to 3 words each from a
40-word vocabulary, 60% broad and 40% exact, and one broad negative per campaign.

| Keywords | Campaigns | Median per iteration (8 queries) | 95% CI of the median | Median ÷ 8 (average per query) |
|---:|---:|---:|---:|---:|
| 1,000 | 100 | 20.07 µs | 19.84 – 20.21 µs | 2.51 µs |
| 10,000 | 1,000 | 198.9 µs | 197.1 – 203.0 µs | 24.9 µs |
| 100,000 | 10,000 | 2.294 ms | 2.245 – 2.352 ms | 287 µs |

### `auction`: one full `Snapshot::run`

One iteration is a complete `Snapshot::run` for the query `free photo editor`
in `IN`. That covers matching, eligibility, ranking by bid × relevance, GSP
pricing and the in-memory budget charge. Every campaign bids on the broad
keyword `photo editor`, so every campaign is a candidate. Budgets are
effectively unlimited, so the first-ranked campaign always pays.

| Candidates | Median | 95% CI of the median |
|---:|---:|---:|
| 10 | 1.261 µs | 1.234 – 1.279 µs |
| 100 | 8.771 µs | 8.603 – 8.870 µs |
| 1,000 | 114.7 µs | 113.2 – 115.5 µs |

### Raw criterion output

Criterion prints `time: [lower estimate upper]`. The middle value is its
slope estimate, or the mean for `match/100000`, where criterion switched to
flat sampling. The outer values are the 95% interval.

```text
auction/candidates/10          time:   [1.2535 µs 1.2687 µs 1.2843 µs]
auction/candidates/100         time:   [8.8165 µs 8.9367 µs 9.0645 µs]
auction/candidates/1000        time:   [113.05 µs 114.10 µs 115.09 µs]
match/query_set_of_8/1000      time:   [19.843 µs 20.102 µs 20.388 µs]
match/query_set_of_8/10000     time:   [198.77 µs 201.79 µs 204.89 µs]
match/query_set_of_8/100000    time:   [2.3344 ms 2.4043 ms 2.4819 ms]
```

Outliers flagged by criterion: 2% (auction/10), 5% (auction/100), 4%
(auction/1000), 2% (match/1000), none (match/10000), 14% (match/100000).

### Reading these numbers

- Matching time grows about 10× for each 10× increase in keywords. In this
  corpus every keyword comes from only 40 words, so each query word appears in
  a fixed share of all keywords. The posting lists therefore grow with the
  corpus, and so does the work per query. The seed data uses a 321-word
  vocabulary with skewed popularity, so its lists look different. Matching on
  the seed corpus has not been benchmarked separately: **TBD**.
- From 100 to 1,000 candidates, the auction's time grows by about 13×. This
  has not been profiled yet: **TBD**.
- These are single-threaded, in-process timings of library calls. They are
  not HTTP request latencies and should not be quoted as such.

## Index build time (observed, not benchmarked)

The server logs how long each index build takes. Two builds were observed with
the release binary on the seed-42 data (9,702 campaigns, 121,488 keywords),
each started from a fresh clone: `took_ms=78` and `took_ms=70`. These are
single log lines. A proper build-time benchmark is **TBD**.

## HTTP load test

**TBD (Milestone 6: Go open-loop load generator).** The `took_us` field in
`/v1/match` responses is the server-side time of one request. It is not a
latency benchmark.
