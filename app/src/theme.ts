// sRGB hex for the Glide theme (§9.6.1): the canvas is not reliable with
// oklch() on every webview, so the hex values of the approved table live
// here. Conversion done as a build step of the document, not at runtime.

export type Tokens = {
  typeNumeric: string;
  typeTemporal: string;
  typeText: string;
  typeBoolean: string;
  typeStructured: string;
  stateNormal: string;
  stateAlt: string;
  stateSelection: string;
  /// Cell modified and not yet saved (--state-modified).
  stateModified: string;
  /// New row not yet saved (--state-new): green, like DBeaver.
  stateNew: string;
  /// Row to delete on save (--state-error): red, struck through.
  stateDeleted: string;
  cellFg: string;
  cellMutedFg: string;
  cellNullFg: string;
  cellBorder: string;
  cellPanelBg: string;
  cellHeaderBg: string;
  cellSelectionBorder: string;
};

export const LIGHT: Tokens = {
  typeNumeric: "#085491",
  typeTemporal: "#5D408A",
  typeText: "#191D24",
  typeBoolean: "#005D6D",
  typeStructured: "#7B4200",
  stateNormal: "#FBFCFD",
  stateAlt: "#F1F4F6",
  stateSelection: "#DBE8FF",
  stateModified: "#F5DBA5",
  stateNew: "#CBE9D0",
  stateDeleted: "#F8D2CC",
  cellFg: "#191D24",
  cellMutedFg: "#565B63",
  cellNullFg: "#565B63",
  cellBorder: "#DCDEE2",
  cellPanelBg: "#FBFCFD",
  cellHeaderBg: "#F3F5F8",
  cellSelectionBorder: "#3760C9",
};

export const DARK: Tokens = {
  typeNumeric: "#7FB6EE",
  typeTemporal: "#BE9DF7",
  typeText: "#E5E8EC",
  typeBoolean: "#1EC5E4",
  typeStructured: "#ED9C55",
  stateNormal: "#101317",
  stateAlt: "#181C21",
  stateSelection: "#202D49",
  stateModified: "#483400",
  stateNew: "#1C4328",
  stateDeleted: "#5B1F1C",
  cellFg: "#E5E8EC",
  cellMutedFg: "#A0A5AC",
  cellNullFg: "#A0A5AC",
  cellBorder: "#25292F",
  cellPanelBg: "#101317",
  cellHeaderBg: "#15181D",
  cellSelectionBorder: "#739BF5",
};

/// The user's theme preference (Settings). "system" = follows the operating
/// system, as it was before one could choose.
export type ThemePref = "system" | "light" | "dark";

const KEY = "alamo.theme";
const systemDark = () => window.matchMedia("(prefers-color-scheme: dark)");

export function themePref(): ThemePref {
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "dark" ? v : "system";
}

/// The effective theme: the chosen one or, on "system", the system's.
export function resolvedTheme(): "light" | "dark" {
  const pref = themePref();
  return pref === "system" ? (systemDark().matches ? "dark" : "light") : pref;
}

/// Writes `data-theme` on <html>: the CSS tokens (tokens.css) follow it.
/// To be called before the first render, so as not to flash in the wrong
/// theme; on "system" it keeps listening for system changes.
export function applyTheme(): void {
  const set = () => {
    document.documentElement.dataset.theme = resolvedTheme();
  };
  set();
  systemDark().addEventListener("change", set);
}

/// Persists the choice and reloads the app, like the language switch: the
/// grid (canvas) and the editor read the tokens once, at creation.
export function setThemePref(pref: ThemePref): void {
  if (pref === themePref()) return;
  if (pref === "system") localStorage.removeItem(KEY);
  else localStorage.setItem(KEY, pref);
  location.reload();
}

export function tokens(): Tokens {
  return resolvedTheme() === "dark" ? DARK : LIGHT;
}
