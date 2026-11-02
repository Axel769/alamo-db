import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./tokens.css";
import "./styles.css";
import { applyTheme } from "./theme";
import { invoke } from "@tauri-apps/api/core";
import { lang } from "./i18n";

// Before rendering: the first frame is born in the right theme already.
applyTheme();
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

// The native menu (macOS) is built in Italian before the webview exists:
// relabel it in the chosen language.
if ("__TAURI_INTERNALS__" in window) {
  invoke("set_menu_lang", { lang: lang() }).catch(() => {});
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
