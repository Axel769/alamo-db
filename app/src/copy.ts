// "Copy" buttons: the text goes to the clipboard and the button says
// "Copied" for a moment, or "Copy failed" (the reason in its tooltip) when
// the system refuses the clipboard. `resetOn` changed (another cell,
// another value) = the button goes back to its label at once.
import { useEffect, useRef, useState } from "react";
import { t } from "./i18n";

export type CopyButton = {
  state: "idle" | "copied" | "failed";
  /// The button's text: `idle` when nothing happened.
  label: (idle: string) => string;
  /// Why the last copy failed, for the tooltip.
  error: string | null;
  copy: (text: string) => void;
};

export function useCopy(resetOn?: unknown): CopyButton {
  const [st, setSt] = useState<{ state: CopyButton["state"]; error: string | null }>({ state: "idle", error: null });
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    window.clearTimeout(timer.current);
    setSt({ state: "idle", error: null });
  }, [resetOn]);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const show = (state: CopyButton["state"], error: string | null, ms: number) => {
    setSt({ state, error });
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setSt({ state: "idle", error: null }), ms);
  };
  return {
    ...st,
    label: (idle) => (st.state === "copied" ? t("Copied") : st.state === "failed" ? t("Copy failed") : idle),
    copy: (text) =>
      void navigator.clipboard.writeText(text).then(
        () => show("copied", null, 1500),
        (e: unknown) => show("failed", e instanceof Error ? e.message : String(e), 4000),
      ),
  };
}
