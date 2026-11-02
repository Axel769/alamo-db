// Row estimate from EXPLAIN in the confirmation dialogs (§9.7): fetch and
// rendering shared between WriteGateDialog and UpdateDialog — best-effort,
// the confirmation never waits for the estimate and without an estimate the
// line simply is not there.
import { useQuery } from "@tanstack/react-query";
import { ipc } from "./ipc";
import { locale, t } from "./i18n";

/// Fetch of the estimate, keyed [connId, sql] like the other fetches of the
/// app (retry already off in the QueryClient defaults). gcTime 0: the
/// estimate lives as long as the dialog showing it — reopening recomputes it.
export function useExplainEstimate(connId: number | null, sql: string) {
  return useQuery({
    queryKey: ["explain", connId, sql],
    queryFn: () => ipc.explainEstimate(connId!, sql),
    enabled: connId !== null && sql !== "",
    staleTime: Infinity,
    gcTime: 0,
  });
}

/// "EXPLAIN estimate" line under the SQL of the dialog. `expectOne`: the
/// UPDATE of cell editing expects ONE row — any divergence is a warn, zero
/// included (row vanished from under our feet, the most interesting case).
export default function ExplainEstimate({
  connId,
  sql,
  expectOne = false,
}: {
  connId: number | null;
  sql: string;
  expectOne?: boolean;
}) {
  const est = useExplainEstimate(connId, sql).data;
  if (est === undefined || est === null) return null;
  const warn = expectOne && est !== 1;
  return (
    <div className={"explain-estimate" + (warn ? " warn" : "")}>
      {t("EXPLAIN estimate: ~{n} rows involved", { n: est.toLocaleString(locale()) })}
      {warn ? " " + t("— expected 1") : ""}
    </div>
  );
}
