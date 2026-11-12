// Last line of defence of the interface: an exception while rendering
// unmounts the whole React tree, and without this the window stays empty
// (on the dark theme: black). Here it shows what failed, the technical
// detail to copy for a bug report, and two ways back. "Try again" remounts
// the interface only: connections and result sets live in Rust, the editor
// documents outside React (§9.2), so nothing of the user's is lost.
import { Component, ErrorInfo, ReactNode } from "react";
import { t } from "./i18n";
import { ipc } from "./ipc";

type State = { error: Error | null; stack: string; copied: boolean };

export default class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null, stack: "", copied: false };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error, copied: false };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ stack: info.componentStack ?? "" });
    console.error("interface crashed", error);
  }

  private copy = async () => {
    const { error, stack } = this.state;
    let diagnostics = "";
    try {
      diagnostics = await ipc.diagnosticsText();
    } catch {
      // Outside Tauri, or the backend is gone too: the error alone.
    }
    const text = [
      `${error?.name}: ${error?.message}`,
      error?.stack ?? "",
      stack.trim(),
      `window ${window.innerWidth}×${window.innerHeight}, ${document.visibilityState}`,
      diagnostics,
    ]
      .filter(Boolean)
      .join("\n\n");
    await navigator.clipboard.writeText(text);
    this.setState({ copied: true });
  };

  render() {
    const { error, copied } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="crash-screen" role="alert">
        <h1>{t("The interface stopped with an error")}</h1>
        <p>
          {t(
            "Your connections are still open. Try again to go back where you were; if it happens again, copy the details and send them to us."
          )}
        </p>
        <pre className="crash-detail">{`${error.name}: ${error.message}`}</pre>
        <div className="crash-actions">
          <button className="btn primary" onClick={() => this.setState({ error: null, stack: "", copied: false })}>
            {t("Try again")}
          </button>
          <button className="btn" onClick={() => void this.copy()}>
            {copied ? t("Copied.") : t("Copy details")}
          </button>
          <button className="btn" onClick={() => location.reload()}>
            {t("Reload (closes the connections)")}
          </button>
        </div>
      </div>
    );
  }
}
