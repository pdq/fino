# Performance snapshots

This opt-in Rust harness measures synthetic workloads. It does not run during
ordinary tests. Fixtures contain generated Elixir text only; Fino never compiles
or runs it. There is no Python runtime dependency.

## Commands

Run from the repository root. Choose generic, stable runner and profile labels;
do not use a username, hostname, local directory name, or customer name.

```sh
mise run perf:run local-macos-arm64 default
mise run perf:snapshot local-macos-arm64 default performance/local-macos-arm64.json
mise run perf:check local-macos-arm64 default performance/local-macos-arm64.json
```

Tasks build both the binary and example with `--release --locked`. The harness
refuses debug builds of itself and obvious debug binary paths. An optional
`--binary /path/to/optimized-artifact` selects a previously built release binary.
This flag asserts that the artifact is optimized: relocated binaries do not
carry reliable Cargo profile metadata. Never pass a renamed debug binary.
Install `trash` to clean up the temporary corpus.

For controlled comparisons without rebuilding an existing artifact:

```sh
cargo build --release --locked --example performance
target/release/examples/performance snapshot local-macos-arm64 default performance/before.json --binary /path/to/old-release-artifact
target/release/examples/performance check local-macos-arm64 default performance/before.json --binary /path/to/new-release-artifact
```

Only `snapshot` creates or replaces a baseline. `run` prints JSON; redirect it if
you want an unadopted measurement. `check` prints current raw JSON to stdout and
comparison summaries to stderr, including on a budget failure. Redirect stdout
to retain current samples. `check` never writes or adopts a baseline. Do not
redirect output to the baseline file: the shell would truncate it before launch.
No fabricated baseline is included: capture one on the intended runner after
reviewing measurements. The parent directory of a baseline must exist.

## Measurements

`performance/fixtures.json` (fixture version 2) fixes 1, 32, and 256 files, eight
units per file, two warmups, eleven raw samples, and four parallel workers.
Three legacy scenarios cover one plain AST query, eight grouped AST rules, and
regex matching with string/comment masking. Their generated sources and rule
bytes are unchanged, so old/new binaries receive the same legacy inputs.

Three additional scenarios exercise only the library linked into the harness:

- `root-stage`: capture identifiers, run a root stage with singleton-block
  unwrapping enabled and comments ignored, and export matching marker findings.
  This source has no wrappers; it measures stage dispatch/export overhead.
- `scoped-condition`: check each captured marker with a root-scoped identifier
  query requiring an existing witness.
- `grapheme-length`: match masked markers and require exactly eleven extended
  grapheme clusters. The markers are ASCII; this measures the grapheme-length
  path, not a representative Unicode workload.

All six scenarios measure API scans and fast/strict compilation. Only the three
legacy cases measure subprocess scans, even when a new binary is selected. An
old `--binary` is **never** asked to parse or evaluate stages, conditions, or
`match_length`. The same sources and expected findings are reused for all cases.
Every scan must return exact expected paths, rule IDs, spans, severity, and
counts. Assertions run outside timed scan spans. The default metric set has
103 entries: one startup, twelve compilation, thirty-six API scan, and fifty-four
legacy subprocess metrics. The corpus fingerprint includes every scenario's
rule/config text and generated source, including the API-only rule fields.

Metric prefixes identify both operation and producer:

| Metric                  | Producer and scope                                                                                                     |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `startup/version`       | Selected `--binary`: process spawn through successful `--version`, including output collection                         |
| `subprocess-wall`       | Selected binary: complete `check --json --timings`, including startup, configuration, scan, output and pipe collection |
| `subprocess-evaluation` | Selected binary: sum of per-rule and shared query-pass cumulative elapsed spans from `--timings`, excluding parsing    |
| `subprocess-parse`      | Selected binary: cumulative parser elapsed spans from `--timings`                                                      |
| `evaluation`            | Library linked into the **harness**, not `--binary`: warmed `Engine::scan` wall time, engine loaded once               |
| `compilation`           | Library linked into the **harness**: config/rule load and compilation, separate fast/grouped and strict modes          |

Scan metrics cover one and four workers. Group compilation uses a fixed
four-worker Rayon pool; subprocesses get the same pool setting. The one-file
subprocess scan exposes tiny end-to-end overhead separately from `--version`.
Compilation uses each scenario's rule set, independent of corpus size.
Extending fixtures requires an explicit new baseline.

These are elapsed/wall durations, **not CPU time**. Cumulative spans across
workers can exceed process wall time. CLI timings have 0.1 ms rounding, so zero
is valid. API scan time includes file reads, parsing, worker-pool setup, rules,
suppression and result assembly; it is not an isolated rule-loop measurement.
Compilation includes reads and destruction of compiled rules. Filesystem caches
are warm; this does not model cold boot, applications, or production workloads.
Do not subtract `--version` from scan wall time to claim pure evaluation cost.

For old/new artifacts with the same harness, **only subprocess and startup
metrics change producer**. API/compilation measurements still use the same
linked library. They must not be presented as old-artifact API timings. Rebuild
the harness at each source revision to compare library changes. Artifact hashes
make this distinction visible.

## Snapshot contract and budgets

The typed JSON contract lives in `examples/performance.rs` (`Measurement`, schema
version 1). Each metric stores `samples_ms`, `median_ms`, `mad_ms` (median absolute
deviation), `min_ms`, and `max_ms`. Checks recompute these summaries. Missing,
unexpected, malformed, nonfinite or negative measurements fail closed. At least
three samples are required; current fixtures require eleven.

Metadata includes fixture version and content hash, runner/profile, OS/arch,
available and selected thread counts, release profile, toolchain, Fino version,
checkout revision, selected binary hash, and harness artifact/source hashes.
Fingerprints use deterministic FNV-1a-64 for change detection, not security or
artifact authenticity. The revision describes the collector checkout; it does
not prove an external binary's source revision. Harness artifact hash identifies
the actual linked executable, including library code.

No external binary paths, temporary paths, hostnames, or automatic machine/user
labels are persisted. Artifact hashes/revision are descriptive and may differ
between builds. The collector source hash must match so changed timing boundaries
cannot silently compare against an old measurement method. Runner/profile,
OS/arch, toolchain/version, thread settings and exact corpus definition/hash must
also match. An unmatched platform fails rather than
passing without a comparison. Use a separate baseline for each host/profile.

`performance/budgets.json` configures default limits:

| Category                                   | Relative increase | Absolute increase |
| ------------------------------------------ | ----------------- | ----------------- |
| Startup and subprocess end-to-end wall     | 25%               | 2 ms              |
| API scan and subprocess evaluation/parsing | 20%               | 1 ms              |
| Config/rule compilation                    | 20%               | 2 ms              |

A regression fails only when its median increase **exceeds both** limits. For a
zero rounded baseline, any positive increase exceeds the relative limit, but
the absolute floor still applies. Invalid budget fields fail closed. Dispersion
is reported, not used to silently widen budgets. Review noisy runs; do not
automatically retry until one passes or replace the baseline on failure.

Run budget/validation tests without benchmarks:

```sh
cargo test --locked --example performance
```

Do not add a threshold gate to a shared CI runner. Once a stable dedicated
runner exists, capture and review its baseline explicitly, commit it, and invoke
`mise run perf:check RUNNER PROFILE BASELINE` on the same runner with matching
tools and workers. Schedule it or make it opt-in; keep regular tests fast.
Rebaseline only as an explicit reviewed change when the runner, toolchain,
workload, version, or intended budget changes.
