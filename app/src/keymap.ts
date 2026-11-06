// Keyboard shortcuts (§16): one registry of the actions a key can trigger,
// one notation for their combos, one dispatcher for the DOM. The SQL editor
// gets the same bindings as a CodeMirror keymap (editor.ts) and the texts
// of the interface (tooltips, menus, palette) read the combo from here, so
// a remapped shortcut is shown everywhere it is mentioned.
//
// Notation: CodeMirror's (`Shift-Mod-Enter`, `Alt-Insert`, `Ctrl-.`, `F5`):
// modifiers `Ctrl`, `Alt`, `Shift`, `Mod` (⌘ on macOS, Ctrl elsewhere) and
// `Meta`, then the key as `KeyboardEvent.key` (letters in lower case,
// `Space` for the space bar). One string serves the DOM dispatcher and the
// editor alike.
//
// Lives outside React (§9.2): the editor and the handlers read it directly.
import { useEffect, useRef } from "react";
import { t } from "./i18n";

export const isMac = /Mac/.test(navigator.platform);

/// Where an action applies. `global` works everywhere; the others only while
/// a SQL tab (`editor`), a table tab (`table`) or a focused grid (`grid`)
/// is in front. Conflicts are checked within a context and against `global`.
export type KeyContext = "global" | "editor" | "table" | "grid";

export interface ActionDef {
  readonly id: string;
  /// English, also the i18n key.
  readonly label: string;
  readonly context: KeyContext;
  /// Alamo's defaults: the same on every platform, `Mod` resolves. An
  /// action can have none (a button only) or more than one combo.
  readonly keys: readonly string[];
}

export const ACTIONS = [
  { id: "palette.toggle", label: "Command palette", context: "global", keys: ["Mod-k"] },
  { id: "sidebar.toggle", label: "Show or hide the sidebar", context: "global", keys: ["Mod-b"] },
  { id: "tab.new_query", label: "New query tab", context: "global", keys: [] },
  { id: "tab.close", label: "Close tab", context: "global", keys: ["Mod-w"] },
  { id: "tab.mru", label: "Last used tab", context: "global", keys: ["Ctrl-Tab"] },
  { id: "tab.prev", label: "Previous tab", context: "global", keys: ["Ctrl-PageUp"] },
  { id: "tab.next", label: "Next tab", context: "global", keys: ["Ctrl-PageDown"] },
  { id: "settings.open", label: "Open settings", context: "global", keys: ["Mod-,"] },
  { id: "run", label: "Run", context: "editor", keys: ["Mod-Enter"] },
  { id: "run.all", label: "Run the whole script", context: "editor", keys: ["Shift-Mod-Enter"] },
  { id: "run.cancel", label: "Cancel the running query", context: "editor", keys: [] },
  { id: "txn.commit", label: "Commit", context: "editor", keys: [] },
  { id: "txn.rollback", label: "Rollback", context: "editor", keys: [] },
  { id: "editor.complete", label: "Autocomplete", context: "editor", keys: ["Ctrl-."] },
  { id: "editor.search", label: "Find in the SQL", context: "editor", keys: ["Mod-f"] },
  { id: "editor.toggle_comment", label: "Comment or uncomment the line", context: "editor", keys: ["Mod-/"] },
  { id: "table.save", label: "Save the changes", context: "table", keys: ["Mod-s"] },
  { id: "table.discard", label: "Discard the changes", context: "table", keys: [] },
  { id: "table.where", label: "Go to the WHERE condition", context: "table", keys: ["Mod-f"] },
  { id: "table.filter_add", label: "Add a filter", context: "table", keys: ["Shift-Mod-f"] },
  { id: "table.reload", label: "Reload", context: "table", keys: ["F5", "Mod-r"] },
  { id: "table.back", label: "Back", context: "table", keys: ["Alt-ArrowLeft"] },
  { id: "table.forward", label: "Forward", context: "table", keys: ["Alt-ArrowRight"] },
  { id: "grid.row_add", label: "Add row", context: "grid", keys: ["Alt-Insert"] },
  { id: "grid.row_duplicate", label: "Duplicate row", context: "grid", keys: ["Ctrl-Alt-Insert"] },
  { id: "grid.row_delete", label: "Delete row", context: "grid", keys: ["Delete", "Backspace"] },
  { id: "grid.set_null", label: "Set NULL", context: "grid", keys: ["Shift-Delete", "Shift-Backspace"] },
  { id: "grid.edit_in_panel", label: "Edit the cell in the panel", context: "grid", keys: ["Shift-Enter"] },
] as const satisfies readonly ActionDef[];

export type ActionId = (typeof ACTIONS)[number]["id"];

export function actionDef(id: ActionId): ActionDef {
  return ACTIONS.find((a) => a.id === id)!;
}

/// A combo with `Mod` resolved for this platform.
export interface Combo {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  /// `KeyboardEvent.key`, letters in lower case, `Space` for " ".
  key: string;
}

const MODIFIERS: Record<string, keyof Omit<Combo, "key">> = {
  ctrl: "ctrl",
  control: "ctrl",
  alt: "alt",
  option: "alt",
  shift: "shift",
  meta: "meta",
  cmd: "meta",
  command: "meta",
  mod: isMac ? "meta" : "ctrl",
};

/// `null` when the text is not a combo (unknown modifier, no key).
export function parseCombo(text: string): Combo | null {
  const parts = text.split("-");
  // A trailing "-" is the minus key itself ("Mod--").
  if (parts.length > 1 && parts[parts.length - 1] === "") {
    parts.pop();
    parts[parts.length - 1] = "-";
  }
  const key = parts.pop();
  if (!key) return null;
  const c: Combo = { ctrl: false, alt: false, shift: false, meta: false, key: normalizeKey(key) };
  for (const p of parts) {
    const m = MODIFIERS[p.toLowerCase()];
    if (!m) return null;
    c[m] = true;
  }
  return c;
}

function normalizeKey(key: string): string {
  if (key === " ") return "Space";
  if (key === "Help") return "Insert"; // macOS extended keyboards: same key
  return key.length === 1 ? key.toLowerCase() : key;
}

/// The combo a key event is, or `null` for a modifier alone.
export function comboOf(e: KeyboardEvent): Combo | null {
  let key = e.key;
  if (key === "Control" || key === "Alt" || key === "Shift" || key === "Meta" || key === "Dead" || key === "Unidentified") return null;
  // With ⌥ held macOS reports the typed symbol (⌥X is "≈"): the letter or
  // digit comes from the physical key.
  if (e.altKey) {
    const m = /^(?:Key|Digit)(\w)$/.exec(e.code);
    if (m) key = m[1];
  }
  return { ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey, key: normalizeKey(key) };
}

const comboKey = (c: Combo) => `${+c.ctrl}${+c.alt}${+c.shift}${+c.meta}:${c.key}`;

/// The combo back in the registry's notation, `Mod` for this platform's
/// primary modifier (⌘ on macOS, Ctrl elsewhere): what a recorded shortcut
/// is saved as, so the settings file reads the same on every OS.
export function comboText(c: Combo): string {
  const out: string[] = [];
  if (isMac ? c.ctrl : c.meta) out.push(isMac ? "Ctrl" : "Meta");
  if (c.alt) out.push("Alt");
  if (c.shift) out.push("Shift");
  if (isMac ? c.meta : c.ctrl) out.push("Mod");
  out.push(c.key);
  return out.join("-");
}

const MAC_KEYS: Record<string, string> = {
  Enter: "↵",
  Backspace: "⌫",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  PageUp: "⇞",
  PageDown: "⇟",
  Escape: "Esc",
  Tab: "⇥",
};
const OTHER_KEYS: Record<string, string> = {
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  PageUp: "PgUp",
  PageDown: "PgDn",
  Escape: "Esc",
};

/// The combo as the interface shows it: glyphs on macOS (⇧⌘↵, ⌥←),
/// `Ctrl+Shift+Enter` elsewhere. Key names the user reads in words (Enter,
/// Del) are translated.
export function formatCombo(text: string): string {
  const c = parseCombo(text);
  if (!c) return text;
  const key = c.key === "Delete" ? t("Del") : c.key === "Enter" && !isMac ? t("Enter") : c.key;
  if (isMac) {
    return (c.ctrl ? "⌃" : "") + (c.alt ? "⌥" : "") + (c.shift ? "⇧" : "") + (c.meta ? "⌘" : "") + (MAC_KEYS[key] ?? (key.length === 1 ? key.toUpperCase() : key));
  }
  const parts: string[] = [];
  if (c.ctrl) parts.push("Ctrl");
  if (c.alt) parts.push("Alt");
  if (c.shift) parts.push("Shift");
  if (c.meta) parts.push("Win");
  parts.push(OTHER_KEYS[key] ?? (key.length === 1 ? key.toUpperCase() : key));
  return parts.join("+");
}

// ---- bindings in force --------------------------------------------------

/// Combos per action in force now: the defaults.
export function bindings(): ReadonlyMap<ActionId, readonly string[]> {
  return DEFAULTS;
}

const DEFAULTS: ReadonlyMap<ActionId, readonly string[]> = new Map(ACTIONS.map((a) => [a.id, a.keys]));

/// Combo → actions, for the dispatcher. Rebuilt when the bindings change.
let byCombo: Map<string, ActionId[]> | null = null;
function index(): Map<string, ActionId[]> {
  if (byCombo) return byCombo;
  byCombo = new Map();
  for (const [id, keys] of bindings()) {
    for (const k of keys) {
      const c = parseCombo(k);
      if (!c) continue;
      const slot = comboKey(c);
      const list = byCombo.get(slot);
      if (list) list.push(id);
      else byCombo.set(slot, [id]);
    }
  }
  return byCombo;
}

/// The first combo of the action, formatted; "" when it has none.
export function keyHint(id: ActionId): string {
  const k = bindings().get(id)?.[0];
  return k ? formatCombo(k) : "";
}

/// `label (combo)`, or the label alone when the action has no combo: the
/// text of a tooltip or a menu entry.
export function withKey(label: string, id: ActionId): string {
  const k = keyHint(id);
  return k ? `${label} (${k})` : label;
}

// ---- dispatcher -----------------------------------------------------------

/// `true` = handled: the event is stopped and nobody else sees it.
export type ActionHandler = (e: KeyboardEvent) => boolean | void;

const handlers = new Map<ActionId, ActionHandler[]>();

/// Registers a handler while a component or a module is interested; the
/// last registered is tried first. Returns the unregister function.
export function bindAction(id: ActionId, h: ActionHandler): () => void {
  let list = handlers.get(id);
  if (!list) handlers.set(id, (list = []));
  list.push(h);
  return () => {
    const l = handlers.get(id);
    if (!l) return;
    const i = l.lastIndexOf(h);
    if (i >= 0) l.splice(i, 1);
  };
}

/// `bindAction` for a component: bound once, the handler always current.
export function useAction(id: ActionId, h: ActionHandler): void {
  const ref = useRef(h);
  ref.current = h;
  useEffect(() => bindAction(id, (e) => ref.current(e)), [id]);
}

/// Grid actions run in the capture phase, before Glide's own keys (Delete,
/// Enter): the others in the bubble phase, after CodeMirror and the
/// dialogs, which stop what they handle.
function dispatch(e: KeyboardEvent, capture: boolean): void {
  if (e.defaultPrevented) return;
  const c = comboOf(e);
  if (!c) return;
  const ids = index().get(comboKey(c));
  if (!ids) return;
  for (const id of ids) {
    if ((actionDef(id).context === "grid") !== capture) continue;
    const list = handlers.get(id);
    if (!list) continue;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i](e)) {
        e.preventDefault();
        if (capture) e.stopPropagation();
        return;
      }
    }
  }
}

let installed = false;
/// One pair of listeners on the window, installed with the app.
export function installKeymap(): void {
  if (installed) return;
  installed = true;
  window.addEventListener("keydown", (e) => dispatch(e, true), true);
  window.addEventListener("keydown", (e) => dispatch(e, false));
}
