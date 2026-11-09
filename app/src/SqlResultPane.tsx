// Result of a statement in a SQL tab (§9.5.1): the grid, and above it
// the strip that says whether the result can be edited and why not, or
// carries the pending changes. The editing machinery is the table tab's
// (`gridEditing.tsx`); what differs is where the columns come from
// (`result_source`) and that a save keeps what was typed.
import { useStore, connById, StatementUi, TabUi } from "./store";
import { t } from "./i18n";
import { StatementPane } from "./ResultPane";
import { BatchDialog, PendingBar, ReadOnlyNote, readOnlyErrorText, reasonText, useGridEditing, useResultSource } from "./gridEditing";

export default function SqlResultPane({
  tab,
  st,
  gridHost,
  gridSize,
}: {
  tab: TabUi;
  st: StatementUi;
  gridHost: React.RefObject<HTMLDivElement>;
  gridSize: { w: number; h: number };
}) {
  const conn = useStore((s) => (tab.connId !== null ? connById(s, tab.connId) : undefined));
  const { source, reason, error } = useResultSource(tab, st);
  const readOnlyProfile = conn?.profile.read_only ?? false;
  // Rows still arriving: the keys are stable (append-only), but a delete
  // would desynchronise the count; everything waits for the end.
  const arriving = st.streaming && !st.budgetPaused && st.outcome === null;
  // Pending changes on another statement of the same tab: one result set
  // at a time (they are keyed by its table).
  const otherResult = tab.edit !== null && tab.edit.resultId !== st.resultId ? tab.edit : null;
  const otherIndex = otherResult ? tab.statements.findIndex((s) => s.resultId === otherResult.resultId) : -1;
  const blocked = readOnlyProfile || arriving || otherResult !== null;
  const e = useGridEditing({
    tab,
    st,
    source,
    blocked,
    stale: false,
    rows: "cells",
    active: true,
    afterSave: "patch",
    reload: () => {},
    localSort: null,
  });

  const note =
    source === null
      ? error
        ? readOnlyErrorText(error)
        : reason
          ? reasonText(reason)
          : null
      : readOnlyProfile
        ? t("Read-only: the profile is read-only")
        : otherResult
          ? t("Unsaved changes in statement {n}: save or discard them before editing this result", {
              n: otherIndex >= 0 ? otherIndex + 1 : "?",
            })
          : arriving
            ? t("Read-only while the rows are arriving")
            : null;
  const target = source ? `${source.schema}.${source.table}` : null;
  const showBar = note !== null || (target !== null && e.nUnsaved > 0);

  return (
    <>
      {showBar && (
        <div className="result-bar">
          {note !== null ? (
            <ReadOnlyNote text={note} />
          ) : (
            <span className="writes-to">
              {t("Writes to {table}", { table: "" })}
              <span className="mono">{target}</span>
            </span>
          )}
          <div className="spacer" />
          <PendingBar e={e} running={tab.running} />
        </div>
      )}
      <StatementPane tab={tab} st={st} gridHost={gridHost} gridSize={gridSize} {...e.paneProps} editable={false} />
      {e.batch && target && (
        <BatchDialog
          batch={e.batch}
          target={target}
          profile={conn?.profile ?? null}
          manualCommit={conn?.manualCommit ?? false}
          onRun={() => void e.runBatch()}
          onClose={e.closeBatch}
        />
      )}
    </>
  );
}
