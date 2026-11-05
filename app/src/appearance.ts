// Appearance settings (§16) as the editor and the grid consume them:
// the font of a text field is a CSS family list, the density a row
// height. A font left blank falls back to the default stack, so an empty
// field never gives an unreadable editor or an invalid canvas font.
import { DEFAULTS, Settings } from "./settingsSchema";

export function editorFont(s: Settings): string {
  return s.appearance.editor_font.trim() || DEFAULTS.appearance.editor_font;
}

export function gridFont(s: Settings): string {
  return s.appearance.grid_font.trim() || DEFAULTS.appearance.grid_font;
}

/// Row height of the grid for the chosen density (§9.6.2 presets).
export function gridRowHeight(s: Settings): number {
  return s.appearance.grid_density === "comfortable" ? 30 : 26;
}
