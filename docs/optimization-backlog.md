# Optimization backlog

Items born from the yellow bands of §15: the measurement is recorded, work
goes on, the item is picked up again when the current milestone allows it.

## 1. GC pauses during prolonged canvas scroll (from M0 phase 2)

**Recorded:** 2026-07-29 · **Decision:** de facto yellow, no plan B (§1.2) —
accepted by the project owner: a constant 40 s fling is an edge case, not
common usage.

**Measurement** (Apple M3 Pro, macOS 26.5.2, WKWebView, 60 Hz, median over
5/3 reps, details in `m0-phase2-results.md`):

| Profile | frames >50 ms | max |
|---|---|---|
| Constant 40 s fling (limit) | 7 out of ~5,080 | 262 ms |
| Momentum + pauses (realistic) | 1 out of ~2,700 | 77 ms |

**Isolated cause:** JavaScriptCore GC fed by the allocations of Glide's draw
path (~1 stall every 80k px drawn; independent of IPC, cache and position).
Scroll pauses enable opportunistic GC and reduce jank by ~4×.

**Leads, in order of cost:**
1. Reduce per-cell allocations in the draw on our side (already done:
   `GridCell` memoization, `displayData` clamp, damage limited to the visible
   region).
2. Profile Glide's draw with Web Inspector (Timelines → JavaScript
   Allocations) and patch the hot spots; consider upstreaming.
3. If more were ever needed: custom `drawCell` with zero allocations for the
   simple columns.

**Closure criterion:** zero frames >50 ms in the human profile and max <150 ms
in the continuous profile, on macOS and Windows.

**When to pick it up again:** at the latest with the real grid of M1 — the
`app/` harness remains the test bench (`ALAMO_M0_AUTO=1`).
