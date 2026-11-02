// Space after an accepted completion, as in DBeaver: `user` → pick
// `username` → `username |`, ready for the next word. Only at the end of the
// line: in the middle of text the next character is already there.
//
// The space is "soft": if the next key is punctuation that sticks to the
// name (`.` of `db.table`, `,`, `(` of a function — MySQL rejects `COUNT (`)
// it takes the space's place; a typed space is swallowed, so there is never
// a double one. Any other edit or cursor move makes it a normal space.
import { pickedCompletion } from "@codemirror/autocomplete";
import { EditorState, Extension, StateEffect, StateField } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

const setSoft = StateEffect.define<number | null>();

/// Position right after the soft space, null when there is none.
const softSpace = StateField.define<number | null>({
  create: () => null,
  update(value, tr) {
    for (const e of tr.effects) if (e.is(setSoft)) return e.value;
    return tr.docChanged || tr.selection ? null : value;
  },
});

const STICKY = /^[.,;:()[\]]$/;

export function completionSpace(): Extension {
  return [
    softSpace,
    // Same transaction as the completion: one undo step removes both.
    EditorState.transactionFilter.of((tr) => {
      if (!tr.docChanged || !tr.annotation(pickedCompletion)) return tr;
      const sel = tr.newSelection.main;
      const next = tr.newDoc.sliceString(sel.head, sel.head + 1);
      if (!sel.empty || (next !== "" && next !== "\n")) return tr;
      return [
        tr,
        {
          changes: { from: sel.head, insert: " " },
          selection: { anchor: sel.head + 1 },
          effects: setSoft.of(sel.head + 1),
          sequential: true,
        },
      ];
    }),
    EditorView.inputHandler.of((view, from, to, text) => {
      const at = view.state.field(softSpace);
      if (at === null || to !== at) return false;
      // A space typed after the trailing one reaches here as a rewrite of
      // both: the webview turns the last space into a non-breaking one.
      if (from === at - 1 && /^[  ]/.test(text)) text = text.slice(1);
      else if (from !== at) return false;
      if (/^[  ]$/.test(text)) {
        view.dispatch({ effects: setSoft.of(null) });
        return true;
      }
      if (!STICKY.test(text)) return false;
      view.dispatch({ changes: { from: at - 1, to: at, insert: text }, selection: { anchor: at }, userEvent: "input.type" });
      return true;
    }),
  ];
}
