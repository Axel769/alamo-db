// Rows of the table tab's grid: the rows of the view with the new (unsaved)
// rows among them, each right below the row it was added under, like
// DBeaver. Pure index arithmetic, called for every painted cell: no
// allocation per call, a binary search over the new rows only.

/// Where a new row goes: below view row `after` (-1 = at the top), at
/// position `index` among the new rows.
export type NewRowSlot = { after: number; index: number };

export type RowLayout = {
  /// Rows on screen: the view's plus the new ones.
  count: number;
  /// Screen row → index of the new row there, -1 if it is a view row.
  extraAt(row: number): number;
  /// Screen row (not a new one) → row of the view.
  baseAt(row: number): number;
  /// Row of the view → screen row.
  rowOfBase(base: number): number;
  /// New row `i` → screen row.
  rowOfExtra(i: number): number;
  /// Slot of a new row added below screen row `row` (-1 = at the top).
  below(row: number): NewRowSlot;
};

/// `after[i]`: view row the new row `i` follows, non-decreasing (the
/// order of the new rows on screen). Clamped to the view: rows reloaded
/// fewer keep their new rows at the bottom.
export function rowLayout(base: number, after: readonly number[]): RowLayout {
  const n = after.length;
  const anchor = (i: number) => Math.min(Math.max(after[i], -1), base - 1);
  // Screen row of the new row i: below its anchor and the new rows before it.
  const pos = (i: number) => anchor(i) + 1 + i;
  /// First new row with pos >= row.
  const firstAtOrAfter = (row: number) => {
    let lo = 0;
    let hi = n;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (pos(mid) < row) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  /// First new row with anchor >= base row v: those before it sit above v.
  const firstAnchoredFrom = (v: number) => {
    let lo = 0;
    let hi = n;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (anchor(mid) < v) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  const extraAt = (row: number) => {
    if (n === 0) return -1;
    const i = firstAtOrAfter(row);
    return i < n && pos(i) === row ? i : -1;
  };
  const baseAt = (row: number) => (n === 0 ? row : row - firstAtOrAfter(row));
  return {
    count: base + n,
    extraAt,
    baseAt,
    rowOfBase: (v) => (n === 0 ? v : v + firstAnchoredFrom(v)),
    rowOfExtra: pos,
    below: (row) => {
      if (row < 0) return { after: -1, index: 0 };
      const i = extraAt(row);
      if (i >= 0) return { after: anchor(i), index: i + 1 };
      const v = baseAt(row);
      return { after: v, index: firstAnchoredFrom(v) };
    },
  };
}
