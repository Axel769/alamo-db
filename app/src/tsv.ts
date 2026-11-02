// TSV of the grid clipboard, as Excel writes and reads it: a field with a
// tab, a newline or quotes goes between quotes, with the inner quotes
// doubled. Text only: no HTML in either direction (see GridView).

export function toTsv(rows: readonly (readonly string[])[]): string {
  return rows
    .map((r) => r.map((f) => (/[\t\n\r"]/.test(f) ? `"${f.replace(/"/g, '""')}"` : f)).join("\t"))
    .join("\n");
}

export function parseTsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === '"' && field === "") {
      // Quoted field: up to the quote that is not doubled.
      i++;
      while (i < text.length) {
        if (text[i] === '"') {
          if (text[i + 1] === '"') {
            field += '"';
            i += 2;
            continue;
          }
          i++;
          break;
        }
        field += text[i++];
      }
      continue;
    }
    if (c === "\t") {
      row.push(field);
      field = "";
    } else if (c === "\n" || c === "\r") {
      if (c === "\r" && text[i + 1] === "\n") i++;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else {
      field += c;
    }
    i++;
  }
  // The last row without a trailing newline; the trailing newline (Excel)
  // does not open an empty row.
  if (field !== "" || row.length > 0) {
    row.push(field);
    rows.push(row);
  }
  return rows;
}

/// What a text pasted into the grid becomes (decision 12). It is split
/// into rows and columns only if it really is a table: at least two columns
/// and all rows of the same width (as it comes out of Excel or of this
/// grid), or if the user selected more than one target cell. A multi-line
/// text without structure (a JSON, a note) pasted on ONE cell stays in that
/// cell: split by lines it ended up in the cells below. Even a JSON
/// indented with tabs has rows of different widths.
export function pasteValues(text: string, targetCells: number): string[][] {
  const rows = parseTsv(text);
  if (rows.length <= 1 || targetCells > 1) return rows;
  const width = rows[0].length;
  const isTable = width >= 2 && rows.every((r) => r.length === width);
  return isTable ? rows : [[text]];
}
