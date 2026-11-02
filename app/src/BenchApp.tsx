// M0 harness (§15 phase 2): grid scrolling over the canonical dataset, frame
// time measured. Active only with ALAMO_M0_AUTO=1 / harness profile.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  DataEditor,
  GridCell,
  GridCellKind,
  GridColumn,
  Item,
  DataEditorRef,
} from "@glideapps/glide-data-grid";
import "@glideapps/glide-data-grid/dist/index.css";
import { invoke } from "@tauri-apps/api/core";
import { BLOCK, cacheFor } from "./rowCache";
import { findScroller, runHumanHarness, runScrollHarness, FrameReport } from "./harness";

const QUERY = "SELECT * FROM bench_wide ORDER BY id LIMIT 20000";
const ROW_HEIGHT = 33;

type QueryOut = {
  conn_id: number;
  result_id: number;
  rows: number;
  cols: { name: string; type_display: string }[];
  time_to_first_row_ms: number;
  total_ms: number;
};

const WIDTHS: Record<string, number> = {
  id: 90,
  parent_id: 100,
  uid: 280,
  code: 130,
  name: 190,
  description: 320,
  amount: 120,
  payload: 320,
  tags: 220,
};

export default function BenchApp() {
  const gridRef = useRef<DataEditorRef | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [data, setData] = useState<QueryOut | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [report, setReport] = useState<FrameReport | null>(null);
  const [size, setSize] = useState({ w: window.innerWidth, h: window.innerHeight });
  const cellMemo = useRef(new Map<number, GridCell>());
  const regionRef = useRef({ y: 0, height: 60 });

  useEffect(() => {
    const onResize = () => setSize({ w: window.innerWidth, h: window.innerHeight });
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    invoke<QueryOut>("bench_run_query", { sql: QUERY })
      .then((q) => {
        const cache = cacheFor(q.conn_id, q.result_id, q.cols.length);
        cache.onBlock = (_b, w) => {
          const vis = regionRef.current;
          const from = Math.max(w.row_start, vis.y - 30);
          const to = Math.min(w.row_end, vis.y + vis.height + 30);
          if (from >= to) return;
          const cells: { cell: Item }[] = [];
          for (let r = from; r < to; r++)
            for (let c = 0; c < q.cols.length; c++) cells.push({ cell: [c, r] });
          gridRef.current?.updateCells(cells);
        };
        setData(q);
      })
      .catch((e) => setErr(String(e)));
  }, []);

  useEffect(() => {
    if (!data) return;
    let cancelled = false;
    (async () => {
      const mode = await invoke<{
        auto: boolean;
        preload: boolean;
        fling: number;
        profile: string;
      }>("harness_mode");
      if (!mode.auto || cancelled) return;
      await new Promise((r) => setTimeout(r, 1500));
      if (cancelled) return;
      if (mode.preload) {
        await cacheFor(data.conn_id, data.result_id, data.cols.length).preloadAll(data.rows);
        await new Promise((r) => setTimeout(r, 500));
        if (cancelled) return;
      }
      const scroller = wrapRef.current ? findScroller(wrapRef.current) : null;
      if (!scroller) {
        await invoke("report_error", { message: "Glide scroller not found" });
        return;
      }
      const rep =
        mode.profile === "umano"
          ? await runHumanHarness(scroller)
          : await runScrollHarness(scroller, mode.fling);
      setReport(rep);
      await invoke("report_frames", { report: rep });
    })();
    return () => {
      cancelled = true;
    };
  }, [data]);

  const columns = useMemo<GridColumn[]>(
    () => (data?.cols ?? []).map((c) => ({ title: c.name, width: WIDTHS[c.name] ?? 130 })),
    [data]
  );

  const getCellContent = useCallback(
    (cell: Item): GridCell => {
      if (!data) return LOADING;
      const [col, row] = cell;
      const key = row * data.cols.length + col;
      const memo = cellMemo.current.get(key);
      if (memo !== undefined) return memo;
      const cache = cacheFor(data.conn_id, data.result_id, data.cols.length);
      const v = cache.cell(row, col);
      if (v === undefined) {
        cache.request(row);
        return LOADING;
      }
      const shown = v.isNull ? "NULL" : v.v.length > 80 ? v.v.slice(0, 80) : v.v;
      const out: GridCell = {
        kind: GridCellKind.Text,
        data: v.isNull ? "" : v.v,
        displayData: shown,
        allowOverlay: false,
        themeOverride: v.isNull ? NULL_THEME : undefined,
      };
      cellMemo.current.set(key, out);
      return out;
    },
    [data]
  );

  if (err) return <pre className="msg msg-error">{err}</pre>;
  if (!data) return <div className="msg">connecting and running the query…</div>;

  return (
    <div ref={wrapRef} className="fill">
      <DataEditor
        ref={gridRef}
        columns={columns}
        rows={data.rows}
        getCellContent={getCellContent}
        rowHeight={ROW_HEIGHT}
        smoothScrollX
        smoothScrollY
        rowMarkers="number"
        width={size.w}
        height={size.h}
        onVisibleRegionChanged={(r) => {
          regionRef.current = { y: r.y, height: r.height };
          const cache = cacheFor(data.conn_id, data.result_id, data.cols.length);
          const from = Math.max(0, r.y - BLOCK);
          const to = Math.min(data.rows, r.y + r.height + BLOCK);
          for (let row = from; row < to; row += BLOCK) cache.request(row);
        }}
      />
      {report && (
        <div className="report-overlay">
          {`frames: ${report.frames}  duration: ${(report.duration_ms / 1000).toFixed(1)} s
p50 ${report.p50_ms.toFixed(2)} ms  p95 ${report.p95_ms.toFixed(2)} ms  p99 ${report.p99_ms.toFixed(2)} ms  max ${report.max_ms.toFixed(1)} ms
> 16.6 ms: ${report.over_16_6}   > 50 ms: ${report.over_50}`}
        </div>
      )}
    </div>
  );
}

const LOADING: GridCell = { kind: GridCellKind.Loading, allowOverlay: false };
const NULL_THEME = Object.freeze({ textDark: "#8a8a8a" });
