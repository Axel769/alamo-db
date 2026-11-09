// sRGB hex for the Glide theme (§9.6.1): the canvas is not reliable with
// oklch() on every webview, so the hex values of tokens.css live here,
// printed by check_tokens.py (see §9.6.1), never converted at runtime.

import { setting, useSettings } from "./settings";
import { SettingValues } from "./settingsSchema";

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
  /// --cell-divider (≥3:1): the grid lines of the high-contrast grid (§16).
  cellDivider: string;
  cellPanelBg: string;
  cellHeaderBg: string;
  cellSelectionBorder: string;
};

export const LIGHT: Tokens = {
  typeNumeric: "#085491",
  typeTemporal: "#5D408A",
  typeText: "#15201B",
  typeBoolean: "#005D6D",
  typeStructured: "#7B4200",
  stateNormal: "#FAFCFB",
  stateAlt: "#F0F5F1",
  stateSelection: "#D1EFD8",
  stateModified: "#F5DBA5",
  stateNew: "#C3EAC9",
  stateDeleted: "#FFD1CD",
  cellFg: "#15201B",
  cellMutedFg: "#4D5C55",
  cellNullFg: "#4D5C55",
  cellBorder: "#DAE0DA",
  cellDivider: "#B4BEB5",
  cellPanelBg: "#FAFCFB",
  cellHeaderBg: "#EDF5EE",
  cellSelectionBorder: "#095B41",
};

export const DARK: Tokens = {
  typeNumeric: "#7FB6EE",
  typeTemporal: "#BE9DF7",
  typeText: "#E4E9E5",
  typeBoolean: "#1EC5E4",
  typeStructured: "#ED9C55",
  stateNormal: "#0C120F",
  stateAlt: "#131C18",
  stateSelection: "#153524",
  stateModified: "#483400",
  stateNew: "#1F4127",
  stateDeleted: "#5C1717",
  cellFg: "#E4E9E5",
  cellMutedFg: "#9DA8A0",
  cellNullFg: "#9DA8A0",
  cellBorder: "#202925",
  cellDivider: "#35403B",
  cellPanelBg: "#0C120F",
  cellHeaderBg: "#101814",
  cellSelectionBorder: "#7FC492",
};

/// The user's theme preference (Settings, §16). "system" = follows the
/// operating system, as it was before one could choose.
export type ThemePref = SettingValues["appearance.theme"];

const systemDark = () => window.matchMedia("(prefers-color-scheme: dark)");

export function themePref(): ThemePref {
  return setting("appearance.theme");
}

/// The effective theme: the chosen one or, on "system", the system's.
export function resolvedTheme(): "light" | "dark" {
  const pref = themePref();
  return pref === "system" ? (systemDark().matches ? "dark" : "light") : pref;
}

/// Writes `data-theme` on <html>: the CSS tokens (tokens.css) follow it.
/// To be called before the first render, so as not to flash in the wrong
/// theme; then it follows the system and the setting. The canvas grid
/// cannot read CSS: it takes `tokens()` again through the store's
/// `resolvedTheme`.
export function applyTheme(): void {
  const set = () => {
    const resolved = resolvedTheme();
    document.documentElement.dataset.theme = resolved;
    if (useSettings.getState().resolvedTheme !== resolved) useSettings.setState({ resolvedTheme: resolved });
  };
  set();
  systemDark().addEventListener("change", set);
  useSettings.subscribe((s, prev) => {
    if (s.settings.appearance.theme !== prev.settings.appearance.theme) set();
  });
}

export function tokens(): Tokens {
  return resolvedTheme() === "dark" ? DARK : LIGHT;
}
