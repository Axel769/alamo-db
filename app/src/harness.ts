// Continuous-scroll harness (§15): drives the Glide scroller via
// requestAnimationFrame and records the deltas between frames. The headline
// metric is "zero frames above 50 ms", not the average.

export type SlowFrame = {
  at_ms: number;
  delta_ms: number;
  scroll_top: number;
  phase: number;
};

export type FrameReport = {
  frames: number;
  duration_ms: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  max_ms: number;
  over_16_6: number;
  over_50: number;
  slow_frames: SlowFrame[];
  phase_p95_ms: number[];
  /// Gaps from an occluded/hidden window (rAF suspended): excluded from the stats.
  suspended_frames: number;
  suspended_ms: number;
};

// Above this threshold a delta is not a slow frame but a suspension of the
// rAF (occluded window, App Nap): no canvas rendering takes 2 s.
const SUSPENSION_MS = 2000;

export function findScroller(root: HTMLElement): HTMLElement | null {
  const named = root.querySelector<HTMLElement>(".dvn-scroller");
  if (named) return named;
  for (const el of Array.from(root.querySelectorAll<HTMLElement>("div"))) {
    if (el.scrollHeight > el.clientHeight * 2) return el;
  }
  return null;
}

type Phase = {
  speed: number;
  frames?: number;
  toEnd?: boolean;
  toStart?: boolean;
};

// Profile: slow reading, sustained scroll, fling to the bottom, return.
function phases(fling: number): Phase[] {
  return [
    { speed: 10, frames: 240 },
    { speed: 60, frames: 480 },
    { speed: fling, toEnd: true },
    { speed: -Math.round(fling * 1.3), toStart: true },
  ];
}

export function runScrollHarness(
  scroller: HTMLElement,
  fling = 260
): Promise<FrameReport> {
  const PHASES = phases(fling);
  return new Promise((resolve) => {
    const deltas: number[] = [];
    const phaseOf: number[] = [];
    const slow: SlowFrame[] = [];
    const t0 = performance.now();
    let last: number | null = null;
    let pi = 0;
    let pf = 0;
    let hiddenSince = 0;
    let suspendedFrames = 0;
    let suspendedMs = 0;
    const maxTop = () => scroller.scrollHeight - scroller.clientHeight;
    const onVis = () => {
      if (document.hidden) hiddenSince = performance.now();
    };
    document.addEventListener("visibilitychange", onVis);

    function step(ts: number) {
      if (last !== null) {
        const d = ts - last;
        const wasHidden = hiddenSince > 0 && hiddenSince >= last;
        if (d > SUSPENSION_MS || wasHidden) {
          suspendedFrames++;
          suspendedMs += d;
          hiddenSince = 0;
        } else {
          deltas.push(d);
          phaseOf.push(pi);
          if (d > 50) {
            slow.push({
              at_ms: ts - t0,
              delta_ms: d,
              scroll_top: scroller.scrollTop,
              phase: pi,
            });
          }
        }
      }
      last = ts;
      const p = PHASES[pi];
      scroller.scrollTop += p.speed;
      pf++;
      const done =
        (p.frames !== undefined && pf >= p.frames) ||
        (p.toEnd === true && scroller.scrollTop >= maxTop() - 1) ||
        (p.toStart === true && scroller.scrollTop <= 1);
      if (done) {
        pi++;
        pf = 0;
      }
      if (pi < PHASES.length) requestAnimationFrame(step);
      else {
        document.removeEventListener("visibilitychange", onVis);
        resolve(
          summarize(
            PHASES.length,
            deltas,
            phaseOf,
            slow,
            performance.now() - t0,
            suspendedFrames,
            suspendedMs
          )
        );
      }
    }
    requestAnimationFrame(step);
  });
}

// "Human" profile ("umano"): fling with decaying momentum and pauses, like the
// trackpad. In the pauses JSC can do opportunistic GC — it is the real use
// case, versus the constant fling that never leaves idle time.
export function runHumanHarness(scroller: HTMLElement): Promise<FrameReport> {
  const V0 = 200;
  const DECAY = 0.97;
  const PAUSE_FRAMES = 24;
  const TOTAL_MS = 45000;
  return new Promise((resolve) => {
    const deltas: number[] = [];
    const phaseOf: number[] = [];
    const slow: SlowFrame[] = [];
    const t0 = performance.now();
    let last: number | null = null;
    let hiddenSince = 0;
    let suspendedFrames = 0;
    let suspendedMs = 0;
    let v = V0;
    let pause = 0;
    let dir = 1;
    const maxTop = () => scroller.scrollHeight - scroller.clientHeight;
    const onVis = () => {
      if (document.hidden) hiddenSince = performance.now();
    };
    document.addEventListener("visibilitychange", onVis);

    function step(ts: number) {
      if (last !== null) {
        const d = ts - last;
        const wasHidden = hiddenSince > 0 && hiddenSince >= last;
        if (d > 2000 || wasHidden) {
          suspendedFrames++;
          suspendedMs += d;
          hiddenSince = 0;
        } else {
          deltas.push(d);
          phaseOf.push(0);
          if (d > 50)
            slow.push({ at_ms: ts - t0, delta_ms: d, scroll_top: scroller.scrollTop, phase: 0 });
        }
      }
      last = ts;
      if (pause > 0) {
        pause--;
      } else {
        scroller.scrollTop += dir * v;
        v *= DECAY;
        if (v < 3) {
          v = V0;
          pause = PAUSE_FRAMES;
          if (scroller.scrollTop >= maxTop() - 5000) dir = -1;
          else if (scroller.scrollTop <= 5000) dir = 1;
        }
      }
      if (ts - t0 < TOTAL_MS) requestAnimationFrame(step);
      else {
        document.removeEventListener("visibilitychange", onVis);
        resolve(
          summarize(1, deltas, phaseOf, slow, performance.now() - t0, suspendedFrames, suspendedMs)
        );
      }
    }
    requestAnimationFrame(step);
  });
}

function pct(sorted: number[], q: number): number {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.max(0, Math.ceil(q * sorted.length) - 1));
  return sorted[i];
}

function summarize(
  nPhases: number,
  deltas: number[],
  phaseOf: number[],
  slow: SlowFrame[],
  duration: number,
  suspendedFrames: number,
  suspendedMs: number
): FrameReport {
  const sorted = [...deltas].sort((a, b) => a - b);
  const sum = deltas.reduce((a, b) => a + b, 0);
  const phase_p95_ms = Array.from({ length: nPhases }, (_p, i) => {
    const ph = deltas.filter((_d, j) => phaseOf[j] === i).sort((a, b) => a - b);
    return pct(ph, 0.95);
  });
  return {
    frames: deltas.length,
    duration_ms: duration,
    avg_ms: deltas.length ? sum / deltas.length : 0,
    p50_ms: pct(sorted, 0.5),
    p95_ms: pct(sorted, 0.95),
    p99_ms: pct(sorted, 0.99),
    max_ms: sorted.length ? sorted[sorted.length - 1] : 0,
    over_16_6: deltas.filter((d) => d > 16.6).length,
    over_50: deltas.filter((d) => d > 50).length,
    slow_frames: slow,
    phase_p95_ms,
    suspended_frames: suspendedFrames,
    suspended_ms: suspendedMs,
  };
}
