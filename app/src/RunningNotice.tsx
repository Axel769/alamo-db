import { useEffect, useState } from "react";
import { t } from "./i18n";
import { IconStop } from "./icons";

/// Seconds since the start of the execution, updated every second. Nothing
/// below one second: a fast query must not flash a number.
export function Elapsed({ startedAt, prefix = "" }: { startedAt: number | null; prefix?: string }) {
  const [, tick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => tick((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, []);
  if (startedAt === null) return null;
  const s = Math.floor((Date.now() - startedAt) / 1000);
  if (s < 1) return null;
  const txt = s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
  return <span className="elapsed">{prefix + txt}</span>;
}

/// In place of the results while the data has not arrived yet: says what
/// it is waiting for, for how long, and offers the cancel right where one
/// looks (when there is something to cancel).
export function RunningNotice({
  label,
  startedAt,
  onCancel,
}: {
  label: string;
  startedAt: number | null;
  onCancel?: () => void;
}) {
  return (
    <div className="running-notice" role="status">
      <span className="spinner big" aria-hidden />
      <span>
        {label} <Elapsed startedAt={startedAt} />
      </span>
      {onCancel && (
        <button className="btn small" onClick={onCancel}>
          <IconStop /> {t("Cancel")}
        </button>
      )}
    </div>
  );
}
