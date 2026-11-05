// Display text of a cell (§8.2): the only place that turns the canonical
// value (what the cache holds, what copy and export give) into what the
// grid draws, from the Data settings (§16). Pure functions: a change of
// settings invalidates the drawn cells, nothing is fetched again. A value
// the formatter does not recognize (a column degraded to text, §4.6) is
// drawn as it is.
import { ColumnOut } from "./ipc";
import { setting } from "./settings";
import { Settings } from "./settingsSchema";

export type DataSettings = Settings["data"];

/// The word drawn for NULL: the setting, or NULL when it is blank (a blank
/// would make NULL and the empty string look the same, against §8.2).
export function nullText(d: DataSettings): string {
  return d.null_text.trim() === "" ? "NULL" : d.null_text;
}

/// Text to draw: cut at `max` characters (beyond that no column would show
/// it, and drawing it all costs at every frame), newlines become "↵" and
/// the indentation that follows them a space, so of a multi-line JSON the
/// start of the content is seen, not just the "{"; tabs become spaces.
export function clampText(s: string, max: number): string {
  let out = s.length > max ? s.slice(0, max) + "…" : s;
  if (/[\n\r\t]/.test(out)) out = out.replace(/[ \t]*\r?\n[ \t]*/g, " ↵ ").replace(/[\r\t]/g, " ");
  return out;
}

/// Value edited in the cell panel rather than in the grid's overlay: what
/// the cell cannot show whole (beyond the characters shown, or on more lines).
export function isLong(s: string, max = setting("data.max_cell_chars")): boolean {
  return s.length > max || /[\r\n]/.test(s);
}

/// The canonical value of a column as the grid draws it: dates, numbers and
/// binaries follow the Data settings, the rest is cut and flattened only.
export function displayText(col: ColumnOut, canonical: string, d: DataSettings, locale: string): string {
  let s = canonical;
  switch (col.kind) {
    case "date":
    case "timestamp":
    case "timestamptz":
      s = formatTemporal(col.kind, canonical, d, locale);
      break;
    case "binary":
      if (d.binary_display === "base64") s = hexToBase64(canonical);
      break;
    default:
      if (d.thousands_separator && (col.semantic_class === "numeric" || col.numeric_text)) {
        s = groupNumber(canonical, locale);
      }
  }
  return clampText(s, d.max_cell_chars);
}

// Canonical `YYYY-MM-DD[ HH:MM:SS[.ffffff]][+00]` (alamo-core fmt.rs);
// years outside 4 digits and negative years are left alone.
const CANON_RE = /^(\d{4})-(\d{2})-(\d{2})(?: (\d{2}):(\d{2}):(\d{2})(\.\d{1,6})?)?(\+00)?$/;

function formatTemporal(kind: "date" | "timestamp" | "timestamptz", canonical: string, d: DataSettings, locale: string): string {
  const local = kind === "timestamptz" && d.timestamp_timezone === "local";
  if (d.date_format === "iso" && !local) return canonical;
  const m = CANON_RE.exec(canonical);
  if (!m) return canonical;
  const date = new Date(Date.UTC(2000, 0, 1));
  date.setUTCFullYear(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  date.setUTCHours(Number(m[4] ?? 0), Number(m[5] ?? 0), Number(m[6] ?? 0), 0);
  if (Number.isNaN(date.getTime())) return canonical;
  const withTime = kind !== "date";
  const frac = m[7] ?? "";
  // A timestamp without time zone is a wall-clock value: formatted in UTC
  // it keeps its own fields. With "local" the stored UTC instant is moved,
  // and this computer's offset follows the value so it is not mistaken for
  // the stored one.
  const timeZone = local ? undefined : "UTC";
  const fmt = formatter(d.date_format === "iso" ? "en-CA" : locale, timeZone, withTime);
  const parts = fmt.formatToParts(date);
  const p = (type: string) => parts.find((x) => x.type === type)?.value ?? "";
  const offset = local ? localOffset(date) : "";
  if (d.date_format === "iso") {
    return `${p("year")}-${p("month")}-${p("day")} ${p("hour")}:${p("minute")}:${p("second")}${frac}${offset}`;
  }
  // Intl puts ", " between date and time: a space, as the SQL literal has.
  let out = fmt.format(date).replace(", ", " ");
  if (withTime && frac !== "") {
    // Intl stops at milliseconds: the microseconds go after the seconds.
    const sec = p("second");
    const at = sec === "" ? -1 : out.lastIndexOf(sec);
    out = at < 0 ? out + frac : out.slice(0, at + sec.length) + frac + out.slice(at + sec.length);
  }
  return out + offset;
}

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(locale: string, timeZone: string | undefined, withTime: boolean): Intl.DateTimeFormat {
  const key = `${locale}|${timeZone ?? "local"}|${withTime}`;
  let f = formatters.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, {
      timeZone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      ...(withTime ? { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" } : {}),
    });
    formatters.set(key, f);
  }
  return f;
}

let offsetFmt: Intl.DateTimeFormat | undefined;

/// This computer's offset at that instant, as `+02:00` (`+00:00` on UTC).
function localOffset(date: Date): string {
  offsetFmt ??= new Intl.DateTimeFormat("en-US", { timeZoneName: "longOffset" });
  const name = offsetFmt.formatToParts(date).find((x) => x.type === "timeZoneName")?.value ?? "";
  return name.replace(/^(GMT|UTC)/, "") || "+00:00";
}

// Plain number as the drivers render it: digits, optional sign, fraction
// and exponent. Anything else (NaN, Infinity, text) is left alone.
const NUM_RE = /^(-?)(\d+)(\.(\d+))?([eE][+-]?\d+)?$/;

const separators = new Map<string, { group: string; decimal: string }>();

function groupNumber(canonical: string, locale: string): string {
  const m = NUM_RE.exec(canonical);
  if (!m) return canonical;
  let sep = separators.get(locale);
  if (!sep) {
    const parts = new Intl.NumberFormat(locale).formatToParts(1234567.5);
    sep = {
      group: parts.find((p) => p.type === "group")?.value ?? ",",
      decimal: parts.find((p) => p.type === "decimal")?.value ?? ".",
    };
    separators.set(locale, sep);
  }
  const int = m[2].replace(/\B(?=(\d{3})+(?!\d))/g, sep.group);
  return m[1] + int + (m[4] !== undefined ? sep.decimal + m[4] : "") + (m[5] ?? "");
}

/// `\x0aff…` (the PostgreSQL bytea form every driver renders, §4.4) as
/// base64. A truncated prefix (§8.1) with an odd number of digits drops the
/// last one; text that is not that form stays as it is.
function hexToBase64(canonical: string): string {
  if (!canonical.startsWith("\\x")) return canonical;
  const hex = canonical.slice(2, 2 + ((canonical.length - 2) & ~1));
  if (!/^[0-9a-fA-F]*$/.test(hex)) return canonical;
  let bin = "";
  for (let i = 0; i < hex.length; i += 2) bin += String.fromCharCode(parseInt(hex.slice(i, i + 2), 16));
  return btoa(bin);
}
