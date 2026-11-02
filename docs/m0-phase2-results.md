# M0 — Phases 2-3, results (macOS)

**Date:** 2026-07-29 · **Machine:** Apple M3 Pro, 18 GB, macOS 26.5.2, 60 Hz display
· **Webview:** WKWebView · **Build:** release with fat LTO · **Dataset:** `bench_wide`
(T2), `SELECT * … ORDER BY id LIMIT 20000`.

Harness: `app/` (Tauri 2 + React + Glide Data Grid), scroll driven via
`requestAnimationFrame` with deltas measured on the rAF timestamps. Auto-run
with `ALAMO_M0_AUTO=1`, always-on-top window (WKWebView suspends rAF when
occluded: suspensions are recognized and excluded, counted separately).

## Frame time — the headline metric

Two scroll profiles, 5 and 3 repetitions:

| Profile | p50 | p95 | p99 | frames >50 ms (median) | max (median) | §15 outcome |
|---|---|---|---|---|---|---|
| **Continuous** (constant fling at 15,600 px/s for ~40 s, there and back over all 20k rows) | 17.0 | 17.0 | 18.0 | 7 out of ~5,080 | 262 ms | 🔴 for "zero frames >50" |
| **Human** (fling with decaying momentum + 400 ms pauses, 45 s) | 17.0 | 17.0 | 18.0 | 1 out of ~2,700 | 77 ms | 🟡 |

A note on reading the p95: on a 60 Hz display WKWebView's rAF timestamps are
quantized to the millisecond — a perfect frame (16.67 ms) is recorded as 17.
**p50 = p95 = 17 means "no dropped frame at p95"**; the 16.6 threshold cannot
be measured literally with this instrument.

## Diagnosis of the slow frames

Isolation by exclusion, one run per hypothesis:

1. **It is not IPC/fetch/parse**: with the whole cache preloaded (zero traffic
   during the scroll) the stalls stay identical in position and size.
2. **It is not position**: at half the fling speed the stalls move.
3. **It is cumulative allocation**: the first stall always lands after ~80k px
   drawn, at any speed → **JavaScriptCore GC** fed by the allocations of the
   draw path (mostly internal to Glide).
4. Confirmation: in the human profile the pauses give JSC room for
   opportunistic GC and the stalls nearly disappear (max 82 ms versus 324).

Mitigations already applied in the harness (part of the result above):
`GridCell` memoization (without it: ~2× slow frames), `displayData` truncated
to 80 chars, `updateCells` damage limited to the visible region.

## Reading the red — recommendation

The red triggers **only** on the "zero frames >50 ms" band in the continuous
profile: a constant full-speed fling for 40 seconds without a single idle
frame, which no trackpad produces. In the realistic profile: a single ~77 ms
frame in 45 s of scrolling. p95 at the theoretical limit of the display in
both.

**Recommendation: treat it as a de facto yellow** — an item in the
optimization backlog (reduce per-cell allocations in the draw, possibly patch
or upstream to Glide), do **not** activate plan B of §1.2.

**Decision (2026-07-29): recommendation accepted** — the constant fling is an
edge case, not the common usage of people at work. Item recorded in
`optimization-backlog.md` with a closure criterion.

## Cold start

`main()` → first rAF frame of the webview, release build, 8 launches:
**375–1013 ms, all 🟢** (threshold: <1200).

## Other metrics (phase 1, CLI — already green)

See the run of 2026-07-29: throughput at 101-102% of the read-and-discard
ceiling on T1/T2/T3, RAM ≤1.04×, `fetch_window` p99 ≤0.1 ms, RSS peak ≤1.15×.

## Platform gates (phases 3-4)

| Platform | Status |
|---|---|
| **macOS / WKWebView** | ✅ measured (this document) |
| **Windows / WebView2** | ⏳ **pending, blocking** — needs a machine or Windows CI; the harness is ready (`ALAMO_M0_AUTO=1`, same env vars) |
| **Linux / WebKitGTK** | ⏳ informative, not blocking (§1.2) — half a day with an Ubuntu VM whenever wanted |

## Reproduction

```sh
cd app && pnpm install && pnpm build && cd ..
cargo build --release -p alamo-app
export ALAMO_BENCH_URL='postgres://…/alamo_bench'   # dataset from `alamo-bench prepare`
ALAMO_M0_AUTO=1 caffeinate -dis ./target/release/alamo-app          # continuous profile
ALAMO_M0_AUTO=1 ALAMO_M0_PROFILE=umano caffeinate -dis ./target/release/alamo-app
# ALAMO_M0_PRELOAD=1 → warm cache; ALAMO_M0_FLING=N → fling speed px/frame;
# ALAMO_M0_OUT=path.json → dump of the report
```
