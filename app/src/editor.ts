// CodeMirror 6 editor — the document lives HERE, outside React (§9.2): in the
// store on every keystroke = typing lag on large SQL files.
//
// M2: a single EditorView, one `EditorState` per tab kept in a map outside
// React (§9.2): switching tabs is `setState`, which preserves each tab's
// undo history and cursor.
import {
  autocompletion,
  completionKeymap,
  startCompletion,
  CompletionContext,
  CompletionResult,
} from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentLess, indentMore, toggleComment } from "@codemirror/commands";
import { sql, PostgreSQL, SQLNamespace } from "@codemirror/lang-sql";
import { ensureSyntaxTree, getIndentUnit, HighlightStyle, indentUnit, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { highlightTree, tags } from "@lezer/highlight";
import { highlightSelectionMatches, openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { completionSpace } from "./completionSpace";
import { sqlSyntax } from "./sqlSyntax";
import { lang } from "./i18n";
import { setting, useSettings } from "./settings";
import { Settings, SettingValues } from "./settingsSchema";
import { editorFont } from "./appearance";
import { ActionId, bindings } from "./keymap";
import {
  Compartment,
  countColumn,
  EditorSelection,
  EditorState,
  Extension,
  Prec,
  StateEffect,
  StateField,
} from "@codemirror/state";
import {
  Command,
  Decoration,
  DecorationSet,
  EditorView,
  KeyBinding,
  keymap,
  lineNumbers,
} from "@codemirror/view";

/// Tab as in DBeaver: spaces up to the next indent stop at the cursor; with a
/// selection, the selected lines are indented. Without it the browser moves
/// focus out of the editor (to the result grid).
const insertIndent: Command = (view) => {
  const { state } = view;
  if (state.readOnly) return false;
  if (state.selection.ranges.some((r) => !r.empty)) return indentMore(view);
  const unit = getIndentUnit(state);
  view.dispatch(
    state.changeByRange((r) => {
      const line = state.doc.lineAt(r.head);
      const col = countColumn(line.text.slice(0, r.head - line.from), state.tabSize);
      const insert = " ".repeat(unit - (col % unit));
      return { changes: { from: r.head, insert }, range: EditorSelection.cursor(r.head + insert.length) };
    }),
    { scrollIntoView: true, userEvent: "input.indent" }
  );
  return true;
};

/// Syntax colors: CSS variables only (tokens.css, --syn-*), so the
/// light/dark theme changes them without reconfiguring the editor. Names
/// (tables, columns, aliases) keep the text color: the lang-sql parser
/// does not tell a column from a table, and coloring them all the same
/// and bright would take emphasis away from the keywords.
export const sqlHighlight = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--syn-keyword)", fontWeight: "600" },
  { tag: tags.typeName, color: "var(--syn-type)" },
  { tag: tags.standard(tags.name), color: "var(--syn-builtin)" },
  { tag: [tags.bool, tags.null], color: "var(--syn-keyword)" },
  { tag: tags.number, color: "var(--syn-number)" },
  { tag: tags.string, color: "var(--syn-string)" },
  { tag: tags.special(tags.string), color: "var(--syn-quoted)" },
  { tag: tags.special(tags.name), color: "var(--syn-builtin)" },
  { tag: tags.comment, color: "var(--syn-comment)", fontStyle: "italic" },
  { tag: tags.operator, color: "var(--syn-operator)" },
  { tag: [tags.punctuation, tags.paren, tags.brace, tags.squareBracket], color: "var(--cell-muted-fg)" },
]);

/// Highlighted SQL outside the editor (read-only previews): pieces of
/// text with the classes of `sqlHighlight`. CodeMirror mounts those classes
/// with the first EditorView; if none has been born yet they are mounted
/// here, once.
let highlightMounted = false;
export function sqlPieces(text: string, dialect: string): { text: string; cls: string }[] {
  if (!highlightMounted && sqlHighlight.module) {
    const style = document.createElement("style");
    style.textContent = sqlHighlight.module.getRules();
    document.head.appendChild(style);
    highlightMounted = true;
  }
  const tree = sqlSyntax(dialect).lang.language.parser.parse(text);
  const out: { text: string; cls: string }[] = [];
  let at = 0;
  highlightTree(tree, sqlHighlight, (from, to, cls) => {
    if (from > at) out.push({ text: text.slice(at, from), cls: "" });
    out.push({ text: text.slice(from, to), cls });
    at = to;
  });
  if (at < text.length) out.push({ text: text.slice(at), cls: "" });
  return out;
}

/// Texts of the CodeMirror search panel (Cmd+F) in Italian; in English
/// its own remain (`phrasesExt`).
const SEARCH_PHRASES_IT: Record<string, string> = {
  Find: "Cerca",
  Replace: "Sostituisci",
  next: "successivo",
  previous: "precedente",
  all: "tutti",
  "match case": "maiuscole/minuscole",
  "by word": "parola intera",
  regexp: "regex",
  replace: "sostituisci",
  "replace all": "sostituisci tutti",
  close: "chiudi",
  "current match": "corrispondenza corrente",
  "replaced $ matches": "sostituite $ corrispondenze",
  "replaced match on line $": "sostituita la corrispondenza alla riga $",
  "on line": "alla riga",
};

function phrasesExt(): Extension {
  return lang() === "it" ? EditorState.phrases.of(SEARCH_PHRASES_IT) : [];
}

const setErrorRange = StateEffect.define<{ from: number; to: number } | null>();

// ------------------------------------------------------- current statement

/// What an execution covers: "current" = the selection or the statement
/// under the cursor, "selection" = the selection or everything, "all" =
/// everything. ⌘↵ follows the setting, ⇧⌘↵ is always "all".
export type RunMode = SettingValues["editor.run_shortcut"];
export type Range = { from: number; to: number };
/// What ⌘↩ would do now: the Run button needs it to say so in its label.
export type RunContext = { hasSelection: boolean; statements: number };

/// Statement boundaries (UTF-16 offsets) computed by the Rust splitter —
/// the same one used for execution, so "the query under the cursor" is the
/// one that actually runs. After an edit the boundaries move with the text
/// (`stale`) until the recomputation arrives.
const setRanges = StateEffect.define<Range[]>();

const rangesField = StateField.define<{ ranges: Range[]; stale: boolean }>({
  create: () => ({ ranges: [], stale: true }),
  update(v, tr) {
    for (const e of tr.effects) {
      if (e.is(setRanges)) return { ranges: e.value, stale: false };
    }
    if (!tr.docChanged) return v;
    return {
      ranges: v.ranges.map((r) => ({ from: tr.changes.mapPos(r.from, 1), to: tr.changes.mapPos(r.to, -1) })),
      stale: true,
    };
  },
});

/// The statement "under the cursor": the one containing it, or the one
/// just before if between its end and the cursor there is only whitespace
/// (cursor on the empty line after the `;`), otherwise the first one after the cursor.
function currentStatement(state: EditorState): Range | null {
  const { ranges } = state.field(rangesField);
  if (ranges.length === 0) return null;
  const head = state.selection.main.head;
  let before: Range | null = null;
  for (const r of ranges) {
    if (head >= r.from && head <= r.to) return r;
    if (r.to < head) before = r;
  }
  if (before && /^[\s;]*$/.test(state.doc.sliceString(before.to, head))) return before;
  return ranges.find((r) => r.from > head) ?? before;
}

/// Light background on the lines of the current statement — only when the
/// text has more than one: with a single query it would be noise.
const currentStatementHighlight = EditorView.decorations.compute([rangesField, "selection"], (state) => {
  const { ranges } = state.field(rangesField);
  if (ranges.length < 2 || !state.selection.main.empty) return Decoration.none;
  const cur = currentStatement(state);
  if (!cur) return Decoration.none;
  const decos = [];
  const first = state.doc.lineAt(cur.from).number;
  const last = state.doc.lineAt(cur.to).number;
  for (let n = first; n <= last; n++) {
    decos.push(Decoration.line({ class: "cm-stmt-current" }).range(state.doc.line(n).from));
  }
  return Decoration.set(decos);
});

/// UTF-16 offset → code point (the backend counts in characters).
function utf16ToCodePoint(text: string, u16: number): number {
  return Array.from(text.slice(0, u16)).length;
}

const errorField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setErrorRange)) {
        deco = e.value
          ? Decoration.set([
              Decoration.mark({ class: "cm-sql-error" }).range(e.value.from, e.value.to),
            ])
          : Decoration.none;
      }
    }
    // Any change to the text turns the highlight off: the position is no
    // longer reliable.
    if (tr.docChanged) deco = Decoration.none;
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/// PG reports the position in **characters** (code points); CodeMirror
/// indexes in UTF-16. Converting is mandatory or the highlight slides on
/// non-ASCII text (§6.1 — *the* classic bug of this feature).
function codePointToUtf16(text: string, cpIndex: number): number {
  let cp = 0;
  let u16 = 0;
  for (const ch of text) {
    if (cp >= cpIndex) break;
    cp += 1;
    u16 += ch.length;
  }
  return u16;
}

/// Schema for autocomplete, at module level: applies to every tab of the
/// session (a single connection at M2) and to states created later.
let schemaNs: SQLNamespace | undefined;

/// Dialect context (§2.1): keyword/quoting of the connected engine and the
/// schema in which unqualified names are resolved. On PG it is "public"; on
/// MySQL it is the sticky's current database (the toolbar select), which can
/// change with a USE — without this, autocomplete only offers keywords (the
/// tables live under `<database>`, not under "public").
let langDialect = PostgreSQL;
let langDefaultSchema: string | undefined = "public";

function langExt(): Extension {
  const s = sql({ dialect: langDialect, schema: schemaNs, defaultSchema: langDefaultSchema });
  // The "bare" columns (see columnSource) are added to the lang-sql source
  // on the same language: the results merge in the popup.
  return [s, s.language.data.of({ autocomplete: columnSource })];
}

// ------------------------------------------------- columns from FROM tables
// lang-sql offers columns only after "table." — never bare. Moreover a
// backtick just opened swallows the rest of the line (unclosed token): the
// parser no longer even sees the FROM. This source adds the columns of the
// tables named in the statement (FROM/JOIN/UPDATE/INTO), even inside
// quotes — and precisely because the open quote breaks the tree, the
// references are extracted from the statement TEXT, not from the nodes.

/// Raw catalog schema → table → columns (the same shape Studio passes
/// to setSchema).
let catalogNs: Record<string, Record<string, readonly string[]>> | undefined;

/// `FROM x`, `JOIN x`, `UPDATE x`, `INTO x`, with `x` = name, `sch.name`,
/// quoted or not. `start`/`end` = range of the reference in the text.
type TableRef = { parts: string[]; start: number; end: number };

const REF_HEAD = /\b(?:from|join|update|into)\s+/gi;
const REF_PART = /(`[^`]+`|"[^"]+"|[\w$]+)/y;

function unquote(s: string): string {
  return /^[`"]/.test(s) ? s.slice(1, /[`"]$/.test(s) ? -1 : undefined) : s;
}

/// A table reference starting at `at` (after the keyword or a comma):
/// part, optional `.part`. `null` if there is no name there.
function readRef(text: string, at: number): TableRef | null {
  REF_PART.lastIndex = at;
  const first = REF_PART.exec(text);
  if (!first) return null;
  const parts = [unquote(first[1])];
  let end = REF_PART.lastIndex;
  const dot = /\s*\.\s*/y;
  dot.lastIndex = end;
  if (dot.exec(text)) {
    REF_PART.lastIndex = dot.lastIndex;
    const second = REF_PART.exec(text);
    if (second) {
      parts.push(unquote(second[1]));
      end = REF_PART.lastIndex;
    }
  }
  return { parts, start: at, end };
}

function collectRefs(text: string): TableRef[] {
  const out: TableRef[] = [];
  REF_HEAD.lastIndex = 0;
  for (let m = REF_HEAD.exec(text); m; m = REF_HEAD.exec(text)) {
    let ref = readRef(text, m.index + m[0].length);
    while (ref) {
      out.push(ref);
      // `FROM a, b`: after a reference, the comma introduces another one.
      const comma = /\s*,\s*/y;
      comma.lastIndex = ref.end;
      ref = comma.exec(text) ? readRef(text, comma.lastIndex) : null;
    }
  }
  return out;
}

/// Columns of a reference in the catalog. Unqualified: first the current
/// database, then (single match) the others. Case-insensitive names like
/// MySQL on macOS and typical catalogs.
function refColumns(ref: TableRef): readonly string[] {
  if (!catalogNs) return [];
  const lookup = (schema: string, table: string): readonly string[] => {
    const tables = pickKey(catalogNs!, schema);
    return tables ? (pickKey(tables, table) ?? []) : [];
  };
  if (ref.parts.length >= 2) return lookup(ref.parts[0], ref.parts[1]);
  const table = ref.parts[0];
  if (langDefaultSchema) {
    const cols = lookup(langDefaultSchema, table);
    if (cols.length) return cols;
  }
  // Without a current database (or a same-named table in several databases)
  // the resolution is ambiguous: the union of the columns beats silence — an
  // extra suggestion is ignored, a missing one is typed by hand.
  const hits = Object.keys(catalogNs).filter((s) => pickKey(catalogNs![s], table));
  const out = new Set<string>();
  for (const s of hits) for (const c of lookup(s, table)) out.add(c);
  return [...out];
}

function pickKey<T>(obj: Record<string, T>, key: string): T | undefined {
  if (key in obj) return obj[key];
  const lower = key.toLowerCase();
  const hit = Object.keys(obj).find((k) => k.toLowerCase() === lower);
  return hit !== undefined ? obj[hit] : undefined;
}

function columnSource(context: CompletionContext): CompletionResult | null {
  if (!catalogNs) return null;
  const { state, pos } = context;
  const node = syntaxTree(state).resolveInner(pos, -1);
  if (node.name === "String" || node.name === "LineComment" || node.name === "BlockComment") {
    return null;
  }
  const quoted = node.name === "QuotedIdentifier";
  const ident = quoted || node.name === "Identifier" || node.name === "Keyword";
  if (!ident && !context.explicit) return null;
  const from = ident ? node.from : pos;
  // After "table." qualified columns are lang-sql's job: bail out here.
  if (/\.\s*$/.test(state.sliceDoc(Math.max(0, from - 30), from))) return null;
  // Current statement from the tree; with a broken parse (open quote) the
  // Statement node is there anyway, to be safe degrade to the whole document.
  let stmt = node as typeof node | null;
  while (stmt && stmt.name !== "Statement") stmt = stmt.parent;
  const stmtFrom = stmt?.from ?? 0;
  const text = state.sliceDoc(stmtFrom, stmt?.to ?? state.doc.length);
  const refs = collectRefs(text);
  // The name AFTER the FROM is a table: columns have no place there.
  if (refs.some((r) => stmtFrom + r.start <= from && from <= stmtFrom + r.end)) return null;
  const cols = new Set<string>();
  for (const r of refs) for (const c of refColumns(r)) cols.add(c);
  if (cols.size === 0) return null;
  // boost: the statement's columns above tables and keywords.
  const options = [...cols].map((label) => ({
    label: quoted ? state.sliceDoc(node.from, node.from + 1) + label + state.sliceDoc(node.from, node.from + 1) : label,
    type: "property",
    boost: 2,
  }));
  if (quoted) {
    const close = state.sliceDoc(node.from, node.from + 1);
    const quoteAfter = state.sliceDoc(pos, pos + 1) === close;
    return {
      from: node.from,
      to: quoteAfter ? pos + 1 : undefined,
      options,
      validFor: /^[`'"[]?[\w$]*[`'"\]]?$/,
    };
  }
  return { from, options, validFor: /^[\w$]*$/ };
}

/// Keywords (and type names) in upper case as they are typed, when a
/// space or a bracket closes the word. The syntax tree says what the word
/// is: a `Keyword` inside a string, a comment or a quoted name is not one.
const uppercaseKeywords = EditorView.inputHandler.of((view, from, to, text) => {
  if (from !== to || !/^[\s(),;]$/.test(text)) return false;
  // The parse runs in idle time and may lag behind fast typing: a few ms
  // bring it up to the word just closed.
  const tree = ensureSyntaxTree(view.state, from, 20);
  if (!tree) return false;
  const node = tree.resolveInner(from, -1);
  if ((node.name !== "Keyword" && node.name !== "Type") || node.to !== from) return false;
  const word = view.state.doc.sliceString(node.from, node.to);
  const upper = word.toUpperCase();
  if (upper === word) return false;
  view.dispatch({
    changes: [
      { from: node.from, to: node.to, insert: upper },
      { from, insert: text },
    ],
    selection: { anchor: from + text.length },
    userEvent: "input.type",
    scrollIntoView: true,
  });
  return true;
});

/// The Editor settings (§16) as extensions: completion, indent, wrap and
/// keyword case. Rebuilt on a change of the section.
function editorSettingsExt(): Extension {
  const e = useSettings.getState().settings.editor;
  return [
    autocompletion({
      defaultKeymap: false,
      activateOnTyping: e.autocomplete,
      activateOnTypingDelay: e.autocomplete_delay_ms,
    }),
    EditorState.tabSize.of(e.tab_size),
    indentUnit.of(" ".repeat(e.tab_size)),
    e.line_wrap ? EditorView.lineWrapping : [],
    e.uppercase_keywords ? uppercaseKeywords : [],
  ];
}

const editorSection = (s: Settings) => JSON.stringify(s.editor);
const shortcutsSection = (s: Settings) => JSON.stringify(s.shortcuts);

/// The editor actions of the registry (keymap.ts) as a CodeMirror keymap:
/// Run follows the setting at the moment of the key, Run all is always
/// "all". Highest precedence: a shortcut the user chose beats the built-in
/// bindings. Rebuilt when the bindings change.
function actionKeymap(onRun: (mode: RunMode) => void): Extension {
  const commands: Partial<Record<ActionId, Command>> = {
    run: () => (onRun(setting("editor.run_shortcut")), true),
    "run.all": () => (onRun("all"), true),
    "editor.complete": startCompletion,
    "editor.search": openSearchPanel,
    "editor.toggle_comment": toggleComment,
  };
  const out: KeyBinding[] = [];
  for (const [id, run] of Object.entries(commands) as [ActionId, Command][]) {
    for (const key of bindings().get(id) ?? []) out.push({ key, run });
  }
  return Prec.highest(keymap.of(out));
}

/// The keys the registry owns, taken out of CodeMirror's fixed keymaps so a
/// remapping leaves nothing behind.
const REGISTRY_KEYS = new Set(["Mod-f", "Mod-/"]);
const fixedKeymap = (...maps: readonly (readonly KeyBinding[])[]) => maps.flat().filter((b) => !b.key || !REGISTRY_KEYS.has(b.key));

/// Font and text size of the editor from the settings.
function appearanceExt(): Extension {
  const s = useSettings.getState().settings;
  return EditorView.theme({
    "&": { fontSize: `${s.appearance.editor_font_size}px` },
    ".cm-scroller": { fontFamily: editorFont(s) },
  });
}

export class SqlEditor {
  view: EditorView;
  private themeCompartment = new Compartment();
  private langCompartment = new Compartment();
  /// Search-panel texts in the UI language (§16): reconfigured on a
  /// language change, and on a tab switch (per-state, like the schema).
  private phrasesCompartment = new Compartment();
  /// Font and text size from the settings (§16), applied at once.
  private appearanceCompartment = new Compartment();
  /// The Editor section of the settings (§16): completion, indent, wrap,
  /// keyword case.
  private editorSettingsCompartment = new Compartment();
  /// The shortcuts of the editor actions (keymap.ts), from the registry.
  private keymapCompartment = new Compartment();
  private onRun: (mode: RunMode) => void = () => {};

  /// Who computes the statement boundaries (the Studio, via IPC on the
  /// tab's connection); null = not available (disconnected tab).
  splitter: ((sql: string) => Promise<Range[] | null>) | null = null;
  private splitTimer: number | undefined;
  /// Notification (only on changes) of selection and statement count.
  onContext: ((ctx: RunContext) => void) | null = null;
  private lastContext: RunContext = { hasSelection: false, statements: 0 };

  constructor(parent: HTMLElement, onRun: (mode: RunMode) => void, initialDoc = "", onDocChanged?: () => void) {
    this.view = new EditorView({
      parent,
      state: this.newState(initialDoc, onRun, onDocChanged),
    });
    useSettings.subscribe((s, prev) => {
      if (s.settings.appearance.language !== prev.settings.appearance.language) this.applyPhrases();
      if (
        s.settings.appearance.editor_font !== prev.settings.appearance.editor_font ||
        s.settings.appearance.editor_font_size !== prev.settings.appearance.editor_font_size
      ) {
        this.view.dispatch({ effects: this.appearanceCompartment.reconfigure(appearanceExt()) });
      }
      if (editorSection(s.settings) !== editorSection(prev.settings)) {
        this.view.dispatch({ effects: this.editorSettingsCompartment.reconfigure(editorSettingsExt()) });
      }
      if (shortcutsSection(s.settings) !== shortcutsSection(prev.settings)) this.applyKeymap();
    });
  }

  /// New state for a tab: same extensions as the view (the `Compartment`s
  /// are per-state: after a `setState` the current schema must be reapplied).
  newState(doc: string, onRun: (mode: RunMode) => void, onDocChanged?: () => void): EditorState {
    this.onRun = onRun;
    return EditorState.create({
      doc,
      extensions: [
        lineNumbers(),
        history(),
        // Autosave (§9.4): the notification fires on every change, the
        // debounce is in the caller — the editor knows nothing of persistence.
        EditorView.updateListener.of((u) => {
          if (u.docChanged) {
            onDocChanged?.();
            this.scheduleSplit();
          }
          this.emitContext();
        }),
        rangesField,
        currentStatementHighlight,
        this.langCompartment.of(langExt()),
        syntaxHighlighting(sqlHighlight),
        // Cmd+F: search (and replace) panel above the editor; the other
        // occurrences of the selected text highlight themselves.
        search({ top: true }),
        highlightSelectionMatches(),
        this.phrasesCompartment.of(phrasesExt()),
        this.appearanceCompartment.of(appearanceExt()),
        // Completion keymap without the mac shortcuts `Alt-\`` and `Alt-i`:
        // on non-US layouts `Alt-\`` IS how you type the backtick (MySQL
        // quoting) and CodeMirror would eat it. Ctrl-Space remains — which
        // however on macOS belongs to the system by default (input source
        // switch) and never reaches the editor: the registry's "Autocomplete"
        // (Ctrl-. by default) is the alternative nobody steals.
        this.editorSettingsCompartment.of(editorSettingsExt()),
        completionSpace(),
        Prec.highest(keymap.of(completionKeymap.filter((b) => !b.mac?.startsWith("Alt-")))),
        // Run, Run all, Autocomplete, Find, Comment: from the registry.
        this.keymapCompartment.of(actionKeymap(onRun)),
        errorField,
        keymap.of([{ key: "Tab", run: insertIndent, shift: indentLess }, ...fixedKeymap(defaultKeymap, historyKeymap, searchKeymap)]),
        EditorView.theme({
          "&": { height: "100%" },
          // The default caret is black: in dark mode it disappears (black on black).
          ".cm-content": { caretColor: "var(--cell-fg)" },
          // Line-number gutter: CodeMirror's default is a fixed light gray,
          // a white strip in the dark theme.
          ".cm-gutters": {
            backgroundColor: "var(--cell-header-bg)",
            color: "var(--cell-muted-fg)",
            borderRight: "1px solid var(--cell-border)",
          },
          ".cm-activeLineGutter": { backgroundColor: "var(--state-alt)", color: "var(--cell-fg)" },
          ".cm-activeLine": { backgroundColor: "color-mix(in oklab, var(--state-alt) 55%, transparent)" },
          ".cm-cursor": { borderLeftColor: "var(--cell-fg)" },
          ".cm-stmt-current": { backgroundColor: "color-mix(in oklab, var(--state-selection) 40%, transparent)" },
          ".cm-sql-error": {
            textDecoration: "underline wavy var(--cell-prod, #a02a83)",
            textDecorationColor: "oklch(55% 0.2 25)",
            backgroundColor: "var(--state-error)",
          },
          // Autocomplete tooltip: CodeMirror's defaults are light and in
          // dark mode the text (light --cell-fg) ended up light on white.
          ".cm-tooltip": {
            backgroundColor: "var(--cell-panel-bg)",
            color: "var(--cell-fg)",
            border: "1px solid var(--cell-border)",
            borderRadius: "6px",
            overflow: "hidden",
          },
          ".cm-tooltip.cm-tooltip-autocomplete > ul > li": {
            color: "var(--cell-fg)",
          },
          ".cm-tooltip.cm-tooltip-autocomplete > ul > li[aria-selected]": {
            backgroundColor: "var(--state-selection)",
            color: "var(--cell-fg)",
          },
          ".cm-completionIcon": {
            color: "var(--cell-muted-fg)",
          },
          // Search panel: CodeMirror's defaults are fixed light colors.
          ".cm-panels": {
            backgroundColor: "var(--cell-header-bg)",
            color: "var(--cell-fg)",
          },
          ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--cell-border)" },
          ".cm-panel.cm-search": { padding: "6px 8px", fontSize: "12.5px" },
          ".cm-panel.cm-search input, .cm-panel.cm-search button": {
            font: "inherit",
            fontSize: "12.5px",
            color: "var(--cell-fg)",
            backgroundColor: "var(--cell-panel-bg)",
            border: "1px solid var(--cell-border)",
            borderRadius: "4px",
            padding: "2px 6px",
            margin: "0 4px 0 0",
          },
          ".cm-panel.cm-search button": { backgroundImage: "none", cursor: "pointer" },
          ".cm-panel.cm-search button:hover": { backgroundColor: "var(--state-alt)" },
          ".cm-panel.cm-search label": { fontSize: "12px", color: "var(--cell-muted-fg)", marginRight: "6px" },
          ".cm-panel.cm-search [name=close]": {
            color: "var(--cell-muted-fg)",
            border: "none",
            backgroundColor: "transparent",
            fontSize: "16px",
          },
          ".cm-searchMatch": {
            backgroundColor: "color-mix(in oklab, var(--state-modified) 70%, transparent)",
            outline: "1px solid color-mix(in oklab, var(--state-modified) 90%, var(--cell-fg))",
          },
          ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--state-modified)" },
          ".cm-selectionMatch": { backgroundColor: "color-mix(in oklab, var(--state-selection) 70%, transparent)" },
        }),
        this.themeCompartment.of([]),
      ],
    });
  }

  getState(): EditorState {
    return this.view.state;
  }

  /// Tab switch: mount the other tab's state and realign the autocomplete
  /// schema (the state may have been born before the catalog data).
  setState(state: EditorState): void {
    this.view.setState(state);
    this.applySchema();
    this.applyPhrases();
    this.applyKeymap();
    if (state.field(rangesField).stale) this.scheduleSplit(0);
    this.emitContext();
  }

  private emitContext(): void {
    const st = this.view.state;
    const ctx = { hasSelection: !st.selection.main.empty, statements: st.field(rangesField).ranges.length };
    if (ctx.hasSelection === this.lastContext.hasSelection && ctx.statements === this.lastContext.statements) return;
    this.lastContext = ctx;
    this.onContext?.(ctx);
  }

  /// Recompute the statement boundaries, debounced on typing.
  /// The result is valid only if the document has not changed meanwhile.
  private scheduleSplit(delay = 250): void {
    window.clearTimeout(this.splitTimer);
    this.splitTimer = window.setTimeout(() => void this.refreshRanges(), delay);
  }

  private async refreshRanges(): Promise<void> {
    if (!this.splitter) return;
    const doc = this.view.state.doc;
    const text = doc.toString();
    let ranges: Range[] | null = null;
    try {
      ranges = await this.splitter(text);
    } catch (e) {
      console.error(e);
      return;
    }
    if (!ranges || this.view.state.doc !== doc) return;
    this.view.dispatch({ effects: setRanges.of(ranges) });
  }

  /// What to run (§6.5): everything, the selection if any, otherwise the
  /// statement under the cursor. `offset` is in code points (like the
  /// backend's error offsets, which are then added to it).
  async sqlToRun(mode: RunMode): Promise<{ sql: string; offset: number } | null> {
    const state = this.view.state;
    const text = state.doc.toString();
    if (mode === "all") return { sql: text, offset: 0 };
    const sel = state.selection.main;
    if (!sel.empty) {
      return { sql: state.doc.sliceString(sel.from, sel.to), offset: utf16ToCodePoint(text, sel.from) };
    }
    if (mode === "selection") return { sql: text, offset: 0 };
    if (state.field(rangesField).stale) {
      window.clearTimeout(this.splitTimer);
      await this.refreshRanges();
    }
    // Boundaries not available (disconnected tab, silent backend): everything,
    // as before — better to run than to do nothing without saying so.
    if (this.view.state.doc !== state.doc) return null;
    if (this.view.state.field(rangesField).stale) return { sql: text, offset: 0 };
    const cur = currentStatement(this.view.state);
    if (!cur) return null;
    return { sql: state.doc.sliceString(cur.from, cur.to), offset: utf16ToCodePoint(text, cur.from) };
  }

  /// New catalog data (§10): reconfigure the view's autocomplete.
  /// `undefined` = no catalog (disconnected tab, M3.5): keywords remain.
  /// The shape is the flat record schema → table → columns: it must be
  /// identical for both lang-sql (SQLNamespace) and columnSource (columns from FROM).
  setSchema(ns: Record<string, Record<string, string[]>> | undefined): void {
    schemaNs = ns;
    catalogNs = ns;
    this.applySchema();
  }

  /// Connection or current-database change: reconfigure the autocomplete's
  /// dialect and default schema.
  setDialectContext(dialect: string, currentDb: string | null): void {
    const syntax = sqlSyntax(dialect);
    langDialect = syntax.lang;
    langDefaultSchema = syntax.defaultSchema ?? currentDb ?? undefined;
    this.applySchema();
  }

  private applySchema(): void {
    this.view.dispatch({ effects: this.langCompartment.reconfigure(langExt()) });
  }

  private applyPhrases(): void {
    this.view.dispatch({ effects: this.phrasesCompartment.reconfigure(phrasesExt()) });
  }

  /// The shortcuts in force (keymap.ts): on a change of the Shortcuts
  /// section, and on a tab switch (the state may have been born under
  /// other bindings).
  private applyKeymap(): void {
    this.view.dispatch({ effects: this.keymapCompartment.reconfigure(actionKeymap(this.onRun)) });
  }

  sql(): string {
    return this.view.state.doc.toString();
  }

  clearError(): void {
    this.view.dispatch({ effects: setErrorRange.of(null) });
  }

  /// Highlight the error. `statementCharOffset` and `positionChar` (1-based)
  /// are in characters; if the position is missing the statement's line is
  /// highlighted (degradation of §6.1).
  showError(statementCharOffset: number, positionChar: number | null): void {
    const text = this.sql();
    const startCp = statementCharOffset + (positionChar !== null ? positionChar - 1 : 0);
    const from = codePointToUtf16(text, startCp);
    let to = from;
    if (positionChar !== null) {
      // Up to the end of the word, at least one character.
      const rest = text.slice(from);
      const m = rest.match(/^[\p{L}\p{N}_."']+/u);
      to = from + Math.max(1, m ? m[0].length : 1);
    } else {
      const line = this.view.state.doc.lineAt(Math.min(from, this.view.state.doc.length));
      to = line.to;
    }
    const clamp = (n: number) => Math.min(n, this.view.state.doc.length);
    this.view.dispatch({
      effects: setErrorRange.of({ from: clamp(from), to: clamp(Math.max(to, from + 1)) }),
      selection: { anchor: clamp(from) },
      scrollIntoView: true,
    });
    this.view.focus();
  }

  focus(): void {
    this.view.focus();
  }

  /// Cmd+F from outside the editor (e.g. focus on the grid): opens the
  /// search in the SQL tab's text anyway.
  openSearch(): void {
    openSearchPanel(this.view);
  }

  destroy(): void {
    this.view.destroy();
  }
}
