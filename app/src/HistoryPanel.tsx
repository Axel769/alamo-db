// Query history panel (M4): entries per day, click = new SQL tab with that
// query. Lookup across tabs — the history records in history.ts, here it is
// only read.
import { Fragment, useState } from "react";
import { clearHistory, useHistory } from "./history";
import { locale, t } from "./i18n";
import { newSqlTab } from "./tabs";
import { IconX } from "./icons";
import type { SidePanelProps } from "./panels";

function dayLabel(at: number): string {
  const d = new Date(at);
  const today = new Date();
  const midnight = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
  if (at >= midnight) return t("today");
  if (at >= midnight - 86_400_000) return t("yesterday");
  return d.toLocaleDateString(locale(), { day: "numeric", month: "long", year: "numeric" });
}

function timeLabel(at: number): string {
  return new Date(at).toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" });
}

/// First non-empty line: in the list the query is recognisable, the full
/// text is in the tooltip and in the tab that opens on click.
function firstLine(sql: string): string {
  return sql.split("\n").find((l) => l.trim() !== "") ?? sql;
}

export default function HistoryPanel({ done }: SidePanelProps) {
  const entries = useHistory((s) => s.entries);
  const [filter, setFilter] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);

  const f = filter.trim().toLowerCase();
  const shown = f
    ? entries.filter(
        (e) => e.sql.toLowerCase().includes(f) || e.profileName.toLowerCase().includes(f)
      )
    : entries;

  let lastDay = "";

  return (
    <aside className="history-panel">
      <div className="tree-head">
        <span>{t("History")}</span>
        {entries.length > 0 && (
          <button
            className={"btn small" + (confirmClear ? " danger" : "")}
            title={t("Clear the history (open tabs are untouched)")}
            onClick={() => {
              // Two-click pattern (like Delete in ConnectDialog): no
              // window.confirm.
              if (!confirmClear) {
                setConfirmClear(true);
                window.setTimeout(() => setConfirmClear(false), 4000);
                return;
              }
              setConfirmClear(false);
              clearHistory();
            }}
          >
            {confirmClear ? t("Confirm?") : t("Clear history")}
          </button>
        )}
      </div>
      <div className="tree-filter">
        <input
          value={filter}
          placeholder={t("filter queries and profiles…")}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => e.key === "Escape" && setFilter("")}
        />
        {filter !== "" && (
          <button className="tree-filter-clear" title={t("Clear the filter")} onClick={() => setFilter("")}>
            <IconX />
          </button>
        )}
      </div>
      <div className="tree-body">
        {shown.length === 0 && (
          <div className="tree-note">
            {entries.length === 0 ? t("Executed queries appear here.") : t("No query matches.")}
          </div>
        )}
        {shown.map((e, i) => {
          const day = dayLabel(e.at);
          const head = day !== lastDay;
          lastDay = day;
          return (
            <Fragment key={`${e.at}-${i}`}>
              {head && <div className="panel-section">{day}</div>}
              <button
                className="hist-entry"
                title={(e.profileName ? e.profileName + "\n" : "") + e.sql}
                onClick={() => {
                  newSqlTab(e.sql);
                  done();
                }}
              >
                <span className="hist-time">{timeLabel(e.at)}</span>
                <span className="swatch" style={{ background: e.color ?? "transparent" }} />
                <span className="hist-sql">{firstLine(e.sql)}</span>
              </button>
            </Fragment>
          );
        })}
      </div>
    </aside>
  );
}
