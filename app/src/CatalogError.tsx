// Failure of a catalog read (tree, database list, tables), §6: what failed and
// what to do in the user's words, Retry, and the server's text one click away,
// copyable. Never only in a tooltip.

import { useState } from "react";
import { asErrorOut } from "./errors";
import { t, tMsg } from "./i18n";
import { ErrorPanel } from "./ResultPane";

type Props = {
  /// What is unavailable ("tree unavailable", "tables unavailable").
  label: string;
  error: unknown;
  retrying: boolean;
  onRetry: () => void;
  className?: string;
};

export default function CatalogError({ label, error, retrying, onRetry, className }: Props) {
  const [details, setDetails] = useState(false);
  const err = asErrorOut(error);
  return (
    <div className={"catalog-error" + (className ? " " + className : "")} role="alert">
      <div className="catalog-error-label">{label}</div>
      {/* The advice when the backend has one, otherwise the server's message. */}
      <div className="catalog-error-text">{tMsg(err.hint ?? err.message)}</div>
      <div className="catalog-error-actions">
        <button className="btn small" disabled={retrying} onClick={onRetry}>
          {retrying ? t("loading…") : t("Retry")}
        </button>
        <button className="btn small" aria-expanded={details} onClick={() => setDetails(!details)}>
          {t("Details")}
        </button>
      </div>
      {details && <ErrorPanel error={err} />}
    </div>
  );
}
