// WHERE bar of the table tab: a small CodeMirror (same syntax highlighting
// as the SQL tab) that suggests the columns of the table with their type,
// plus the keywords of the dialect. Enter applies, Shift+Enter inserts a
// newline (a multi-line pasted text stays intact: joining it would break a
// `-- comment`), Esc cancels. The text lives here while typing; the parent
// receives every change and decides when to apply it.
import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import {
  autocompletion,
  Completion,
  CompletionContext,
  CompletionResult,
  completionKeymap,
  pickedCompletion,
  startCompletion,
} from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, insertNewline } from "@codemirror/commands";
import { keywordCompletionSource, sql } from "@codemirror/lang-sql";
import { syntaxHighlighting } from "@codemirror/language";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, placeholder as placeholderExt } from "@codemirror/view";
import { completionSpace } from "./completionSpace";
import { sqlHighlight } from "./editor";
import { quoteIfNeeded, SqlSyntax, sqlSyntax } from "./sqlSyntax";

export type WhereColumn = { name: string; type_display: string };

/// For the parent: ⌘F puts the cursor at the end of the condition.
export type WhereInputHandle = { focus: () => void };

function columnSource(columns: WhereColumn[], syntax: SqlSyntax) {
  const { quote } = syntax;
  const options: Completion[] = columns.map((c) => ({
    label: c.name,
    detail: c.type_display,
    type: "property",
    boost: 99,
    // Inside an already open quote (`"Nam`) the bare name is completed and
    // the quote closed; otherwise it is quoted only if needed.
    apply: (view, completion, from, to) => {
      const before = view.state.sliceDoc(from - 1, from);
      const after = view.state.sliceDoc(to, to + 1);
      const insert =
        before === quote ? c.name + (after === quote ? "" : quote) : quoteIfNeeded(c.name, syntax);
      view.dispatch({
        changes: { from, to, insert },
        selection: { anchor: from + insert.length + (before === quote && after === quote ? 1 : 0) },
        annotations: pickedCompletion.of(completion),
      });
    },
  }));
  return (ctx: CompletionContext): CompletionResult | null => {
    const word = ctx.matchBefore(/[\w$]*/);
    if (!word || (word.from === word.to && !ctx.explicit)) return null;
    return { from: word.from, options, validFor: /^[\w$]*$/ };
  };
}

type WhereInputProps = {
  value: string;
  columns: WhereColumn[];
  /// `Profile.dialect`: the engine's syntax (`sqlSyntax`).
  dialect: string;
  disabled: boolean;
  placeholder: string;
  title: string;
  onChange: (text: string) => void;
  /// Text read from the editor when Enter is pressed.
  onCommit: (text: string) => void;
  onCancel: () => void;
};

const WhereInput = forwardRef<WhereInputHandle, WhereInputProps>(function WhereInput(
  {
    value,
    columns,
    dialect,
    disabled,
    placeholder,
    title,
    onChange,
    onCommit,
    onCancel,
  },
  ref
) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const langComp = useRef(new Compartment());
  const editComp = useRef(new Compartment());
  // The callbacks change at every render of the parent: the editor always
  // reads the latest ones, without being recreated.
  const cb = useRef({ onChange, onCommit, onCancel });
  cb.current = { onChange, onCommit, onCancel };

  const langExt = () => {
    const syntax = sqlSyntax(dialect);
    const d = syntax.lang;
    return [
      sql({ dialect: d }),
      autocompletion({
        defaultKeymap: false,
        override: [columnSource(columns, syntax), keywordCompletionSource(d, true)],
      }),
      completionSpace(),
    ];
  };
  const editExt = (off: boolean) => [EditorView.editable.of(!off), EditorState.readOnly.of(off)];

  useEffect(() => {
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          history(),
          langComp.current.of(langExt()),
          editComp.current.of(editExt(disabled)),
          syntaxHighlighting(sqlHighlight),
          placeholderExt(placeholder),
          EditorView.lineWrapping,
          EditorView.contentAttributes.of({ spellcheck: "false", autocapitalize: "off", autocorrect: "off" }),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) cb.current.onChange(u.state.doc.toString());
          }),
          // First the popup (Enter picks, Esc closes), then the keys of the
          // bar, then normal editing.
          Prec.highest(
            keymap.of([
              ...completionKeymap.filter((b) => !b.mac?.startsWith("Alt-")),
              { key: "Ctrl-.", run: startCompletion },
            ])
          ),
          Prec.high(
            keymap.of([
              { key: "Enter", run: (v) => (cb.current.onCommit(v.state.doc.toString()), true) },
              { key: "Shift-Enter", run: insertNewline },
              {
                key: "Escape",
                run: (v) => {
                  cb.current.onCancel();
                  v.contentDOM.blur();
                  return true;
                },
              },
            ])
          ),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          EditorView.theme({
            "&": { fontSize: "12px" },
            ".cm-scroller": { fontFamily: "ui-monospace, SFMono-Regular, monospace", maxHeight: "96px" },
            ".cm-content": { caretColor: "var(--cell-fg)", padding: "3px 0" },
            ".cm-line": { padding: "0 6px" },
            ".cm-cursor": { borderLeftColor: "var(--cell-fg)" },
            ".cm-placeholder": { color: "var(--cell-muted-fg)" },
            // Same tooltip as the SQL tab (in dark mode the defaults are light).
            ".cm-tooltip": {
              backgroundColor: "var(--cell-panel-bg)",
              color: "var(--cell-fg)",
              border: "1px solid var(--cell-border)",
              borderRadius: "6px",
              overflow: "hidden",
            },
            ".cm-tooltip.cm-tooltip-autocomplete > ul": { fontFamily: "ui-monospace, SFMono-Regular, monospace" },
            ".cm-tooltip.cm-tooltip-autocomplete > ul > li": { color: "var(--cell-fg)" },
            ".cm-tooltip.cm-tooltip-autocomplete > ul > li[aria-selected]": {
              backgroundColor: "var(--state-selection)",
              color: "var(--cell-fg)",
            },
            ".cm-completionDetail": { color: "var(--cell-muted-fg)", fontStyle: "normal", marginLeft: "1em" },
            ".cm-completionIcon": { color: "var(--cell-muted-fg)" },
          }),
        ],
      }),
    });
    view.current = v;
    return () => {
      v.destroy();
      view.current = null;
    };
    // The editor is born once; value, columns and state are realigned below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Text changed from outside (Esc, ×, Back/Forward): the doc is rewritten.
  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value) {
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } });
    }
  }, [value]);

  useEffect(() => {
    view.current?.dispatch({ effects: langComp.current.reconfigure(langExt()) });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [columns, dialect]);

  useEffect(() => {
    view.current?.dispatch({ effects: editComp.current.reconfigure(editExt(disabled)) });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [disabled]);

  useImperativeHandle(ref, () => ({
    focus: () => {
      const v = view.current;
      if (!v) return;
      v.focus();
      v.dispatch({ selection: { anchor: v.state.doc.length } });
    },
  }));

  return <div ref={host} className="where-input" title={title} />;
});

export default WhereInput;
