// Cache of the row windows — outside React, mutable object with imperative
// redraw (§9.2): in a store every scroll → cascade of re-renders → dropped
// frames.
import { useEffect, useState } from "react";
import { ipc, WindowOut } from "./ipc";

export const BLOCK = 256;
/// Whole values kept (cell panel and row card show one each): the last read.
const FULL_KEEP = 4;

export class RowCache {
  private blocks = new Map<number, WindowOut>();
  /// Blocks being read: whoever needs one waits for the same request.
  private pending = new Map<number, Promise<void>>();
  /// Rows of the result set known at the last `invalidatePartial`: a block
  /// that arrives shorter than this was read before the set grew (next
  /// page arrived during the request).
  private totalRows = 0;
  /// Whole values of cells truncated in the window (`get_cell_full`, §4.7),
  /// by `row:col`. Here and not in the panels: a save re-reads them with
  /// the blocks, before the pending edits go, so the panel never shows the
  /// value from before the save.
  private full = new Map<string, string | null>();
  onBlock: (block: number, w: WindowOut) => void = () => {};

  constructor(
    private connId: number,
    private resultId: number,
    private nCols: number
  ) {}

  cell(row: number, col: number): { v: string; isNull: boolean; truncated: boolean } | undefined {
    const w = this.blocks.get(Math.floor(row / BLOCK));
    if (!w) return undefined;
    const i = row - w.row_start;
    const c = w.columns[col];
    if (!c || i < 0 || i >= c.values.length) return undefined;
    return { v: c.values[i], isNull: c.nulls[i], truncated: c.truncated[i] };
  }

  /// Block already arrived, without requesting it (width measurement).
  peekBlock(b: number): WindowOut | undefined {
    return this.blocks.get(b);
  }

  request(row: number): void {
    void this.fetchBlock(Math.floor(row / BLOCK));
  }

  /// The view changed (sort applied or removed, §4.8): every block is in
  /// the old space, drop everything.
  clear(): void {
    this.blocks.clear();
    this.full.clear();
  }

  /// During streaming a block may have arrived partial (the stream did not
  /// have those rows yet): when the count grows it must be redone. The
  /// removed blocks are requested right away: they were in use (visible or
  /// nearby) and the grid does not request by itself a cell already drawn
  /// as "loading" — it stayed empty until a scroll.
  invalidatePartial(totalRows: number): void {
    this.totalRows = Math.max(this.totalRows, totalRows);
    for (const [b, w] of this.blocks) {
      const expectedEnd = Math.min((b + 1) * BLOCK, totalRows);
      if (w.row_end < expectedEnd) {
        this.blocks.delete(b);
        void this.fetchBlock(b);
      }
    }
  }

  /// Rows changed in Rust (re-read after a save): the blocks already loaded
  /// are requested again. The old stays visible until the new arrives — no
  /// empty cells in the meantime.
  async refreshLoaded(): Promise<void> {
    const full = [...this.full.keys()].map(async (k) => {
      const [row, col] = k.split(":").map(Number);
      try {
        await this.loadFull(row, col);
      } catch {
        this.full.delete(k);
      }
    });
    await Promise.all([
      ...full,
      ...[...this.blocks.keys()].map(async (b) => {
        const w = await ipc.fetchWindow(
          this.connId,
          this.resultId,
          b * BLOCK,
          (b + 1) * BLOCK,
          0,
          this.nCols
        );
        this.blocks.set(b, w);
        this.onBlock(b, w);
      }),
    ]);
  }

  /// Whole value of a cell already read, `undefined` = not read (or
  /// dropped). `null` = NULL.
  fullValue(row: number, col: number): string | null | undefined {
    return this.full.get(`${row}:${col}`);
  }

  async loadFull(row: number, col: number): Promise<void> {
    const v = await ipc.getCellFull(this.connId, this.resultId, row, col);
    const k = `${row}:${col}`;
    this.full.delete(k);
    this.full.set(k, v);
    if (this.full.size > FULL_KEEP) this.full.delete(this.full.keys().next().value!);
  }

  async preloadAll(totalRows: number): Promise<void> {
    for (let b = 0; b * BLOCK < totalRows; b++) await this.fetchBlock(b);
  }

  /// Rows `from..to` in cache (an edit on a whole column): resolves when
  /// their blocks have arrived, or failed.
  async ensure(from: number, to: number): Promise<void> {
    const blocks: Promise<void>[] = [];
    for (let b = Math.floor(from / BLOCK); b * BLOCK < to; b++) blocks.push(this.fetchBlock(b));
    await Promise.all(blocks);
  }

  private fetchBlock(b: number, retried = false): Promise<void> {
    if (this.blocks.has(b)) return Promise.resolve();
    const cur = this.pending.get(b);
    if (cur) return cur;
    const p = this.load(b, retried).finally(() => {
      if (this.pending.get(b) === p) this.pending.delete(b);
    });
    this.pending.set(b, p);
    return p;
  }

  /// `retried`: a single retry per short block. With a local sort the view
  /// may cover fewer rows than the set until it is recomputed: a short
  /// block there is right, and retrying forever would hammer the IPC.
  private async load(b: number, retried: boolean): Promise<void> {
    try {
      const w = await ipc.fetchWindow(
        this.connId,
        this.resultId,
        b * BLOCK,
        (b + 1) * BLOCK,
        0,
        this.nCols
      );
      // Read before the next page arrived: it is already stale. It is
      // requested again instead of kept (the cell stayed "loading" forever:
      // the block was there, only short).
      if (!retried && w.row_end < Math.min((b + 1) * BLOCK, this.totalRows)) {
        this.pending.delete(b);
        return this.fetchBlock(b, true);
      }
      this.blocks.set(b, w);
      this.onBlock(b, w);
    } catch (e) {
      console.error(e);
    }
  }
}

// Registry of the caches per result set, outside React. `release` must be
// called when the result set is released in Rust too. Composite key: the
// result_ids are per-connection counters (M3.5), alone they collide.
const caches = new Map<string, RowCache>();

export function cacheFor(connId: number, resultId: number, nCols: number): RowCache {
  const key = `${connId}:${resultId}`;
  let c = caches.get(key);
  if (!c) {
    c = new RowCache(connId, resultId, nCols);
    caches.set(key, c);
  }
  return c;
}

/// Whole value of a cell for the panels, `null` while it loads. `needed`
/// false = the window value is already whole: nothing is read.
export function useFullValue(cache: RowCache, row: number, col: number, needed: boolean): string | null {
  const [, setLoaded] = useState(0);
  const value = needed ? cache.fullValue(row, col) : undefined;
  const missing = needed && value === undefined;
  useEffect(() => {
    if (!missing) return;
    let alive = true;
    cache
      .loadFull(row, col)
      .then(() => alive && setLoaded((n) => n + 1))
      .catch((e) => console.error(e));
    return () => {
      alive = false;
    };
  }, [cache, row, col, missing]);
  return value ?? null;
}

export function dropCache(connId: number, resultId: number): void {
  caches.delete(`${connId}:${resultId}`);
}
