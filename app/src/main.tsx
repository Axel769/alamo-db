import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./tokens.css";
import "./styles.css";
import { applyTheme } from "./theme";
import { bootSettings } from "./settings";

// macOS: integrated title bar (tauri.conf.json, titleBarStyle
// Overlay) — the toolbar leaves room for the window buttons.
if (/Mac/.test(navigator.platform) && "__TAURI_INTERNALS__" in window) {
  document.documentElement.classList.add("mac-titlebar");
}

// Inside Tauri the right click shows the WebView's native menu: "Reload"
// (reloads the page and throws away connections and editor) and, in debug,
// "Inspect Element". It is turned off where the app has no menu of its own.
// It stays in text fields (system cut/copy/paste) and with ⌥ held, to open
// the devtools when needed.
if ("__TAURI_INTERNALS__" in window) {
  window.addEventListener("contextmenu", (e) => {
    if (e.altKey) return;
    const el = e.target instanceof Element ? e.target : null;
    if (el?.closest("input, textarea, [contenteditable], .cm-content")) return;
    e.preventDefault();
  });
}

// Settings first (§16), then the theme, then the first frame: it is born
// in the right language and theme already. Outside Tauri (plain browser)
// the defaults stand.
async function boot() {
  if ("__TAURI_INTERNALS__" in window) {
    try {
      await bootSettings();
    } catch (e) {
      console.error("settings", e);
    }
  }
  applyTheme();
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}

void boot();
