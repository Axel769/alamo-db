// Server-side sorting of the table tab (decision 10): list of columns in
// priority order, turned into `ORDER BY a, b DESC` by the backend. Here
// only the list transformations, no React: used by the chips, the header
// menu and the header click.
import { OrderBy } from "./ipc";

/// Only this column.
export function sortOnly(column: string, desc: boolean): OrderBy[] {
  return [{ column, desc }];
}

/// Appends the column; if it is already there only its direction changes
/// (it keeps its place in the priority).
export function sortAdd(order: OrderBy[], column: string, desc: boolean): OrderBy[] {
  return order.some((o) => o.column === column)
    ? order.map((o) => (o.column === column ? { column, desc } : o))
    : [...order, { column, desc }];
}

export function sortRemove(order: OrderBy[], column: string): OrderBy[] {
  return order.filter((o) => o.column !== column);
}

/// Reverses the direction of a column already in the sort.
export function sortFlip(order: OrderBy[], column: string): OrderBy[] {
  return order.map((o) => (o.column === column ? { column, desc: !o.desc } : o));
}

/// Header click, like DBeaver. Without Shift: only this column,
/// ↑ → ↓ → no sort. With Shift: the column is appended (↑), then goes
/// to ↓, then leaves the sort; the others stay.
export function sortHeaderClick(order: OrderBy[], column: string, additive: boolean): OrderBy[] {
  const cur = order.find((o) => o.column === column);
  if (additive) {
    if (!cur) return [...order, { column, desc: false }];
    return cur.desc ? sortRemove(order, column) : sortFlip(order, column);
  }
  if (order.length === 1 && cur) return cur.desc ? [] : sortOnly(column, true);
  return sortOnly(column, false);
}

/// Moves the column at position `from` to position `to` (priority
/// changed by dragging the chip); the others keep their order.
export function sortMove(order: OrderBy[], from: number, to: number): OrderBy[] {
  if (from === to || from < 0 || from >= order.length) return order;
  const out = order.filter((_, i) => i !== from);
  out.splice(Math.max(0, Math.min(to, out.length)), 0, order[from]);
  return out;
}
