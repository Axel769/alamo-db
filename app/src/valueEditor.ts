// Editor of the cell panel (InspectPanel): the whole value of one cell,
// editable when the grid is. CodeMirror like the SQL editor (same colors),
// JSON highlighting when the value looks like JSON. The text is never
// reformatted: what is shown is what is in the database.
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { json } from "@codemirror/lang-json";
import { syntaxHighlighting } from "@codemirror/language";
import { search, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState, Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, placeholder } from "@codemirror/view";
import { sqlHighlight } from "./editor";

export type ValueEditorOptions = {
  readOnly: boolean;
  /// Shown with an empty document: "NULL" or "DEFAULT" for those states.
  placeholder: string;
  /// Text typed by the user (never fired by `reset`).
  onChange: (text: string) => void;
  /// Esc: undo the edits made since entering the editor.
  onEscape: () => void;
  /// ⌘↵: done, back to the grid.
  onDone: () => void;
};

/// JSON object or array: highlighted as JSON. Anything else stays plain text.
function looksLikeJson(text: string): boolean {
  const s = text.trimStart();
  return s.startsWith("{") || s.startsWith("[");
}

const theme = EditorView.theme({
  "&": { height: "100%", fontSize: "12px" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "ui-monospace, SFMono-Regular, monospace", lineHeight: "1.5" },
  ".cm-content": { caretColor: "var(--cell-fg)" },
  ".cm-cursor": { borderLeftColor: "var(--cell-fg)" },
  ".cm-gutters": {
    backgroundColor: "var(--cell-header-bg)",
    color: "var(--cell-muted-fg)",
    borderRight: "1px solid var(--cell-border)",
  },
  ".cm-activeLineGutter": { backgroundColor: "transparent" },
  "&.cm-focused .cm-activeLineGutter": { backgroundColor: "var(--state-alt)", color: "var(--cell-fg)" },
  ".cm-placeholder": { color: "var(--cell-muted-fg)", fontStyle: "italic" },
});

export class ValueEditor {
  view: EditorView;
  private opts: ValueEditorOptions;
  private mode = new Compartment();

  constructor(parent: HTMLElement, doc: string, opts: ValueEditorOptions) {
    this.opts = opts;
    this.view = new EditorView({ parent, state: this.newState(doc) });
  }

  /// New document (another cell, or the value changed outside the editor):
  /// a new state, so the undo history does not cross cells.
  reset(doc: string): void {
    this.view.setState(this.newState(doc));
  }

  /// Read-only and placeholder follow the cell without touching the
  /// document: while typing, cursor and undo history stay.
  configure(opts: ValueEditorOptions): void {
    const was = this.opts;
    this.opts = opts;
    if (was.readOnly !== opts.readOnly || was.placeholder !== opts.placeholder)
      this.view.dispatch({ effects: this.mode.reconfigure(this.modeExt()) });
  }

  private modeExt(): Extension {
    return [EditorState.readOnly.of(this.opts.readOnly), placeholder(this.opts.placeholder)];
  }

  get text(): string {
    return this.view.state.doc.toString();
  }

  private newState(doc: string): EditorState {
    const ext: Extension[] = [
      lineNumbers(),
      history(),
      EditorView.lineWrapping,
      this.mode.of(this.modeExt()),
      search({ top: true }),
      syntaxHighlighting(sqlHighlight),
      theme,
      EditorView.updateListener.of((u) => {
        if (u.docChanged && !this.opts.readOnly) this.opts.onChange(u.state.doc.toString());
      }),
      keymap.of([
        {
          key: "Escape",
          run: () => {
            this.opts.onEscape();
            return true;
          },
        },
        {
          key: "Mod-Enter",
          run: () => {
            this.opts.onDone();
            return true;
          },
        },
        ...defaultKeymap,
        ...historyKeymap,
        ...searchKeymap,
      ]),
    ];
    if (looksLikeJson(doc)) ext.push(json());
    return EditorState.create({ doc, extensions: ext });
  }

  destroy(): void {
    this.view.destroy();
  }
}
