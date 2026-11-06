// Shortcuts section of the settings window (§16): the actions of the
// registry (keymap.ts) by context, each with its combo in force. A click
// on the combo records the next key; "Remove" leaves the action without a
// shortcut; an action changed from the preset is marked and goes back
// with its own button. Two actions on one key in the same context (or one
// of them global) are both flagged, in red, and both kept: the user
// decides, as in DBeaver. Found by the search like a setting.
import { useEffect, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { searchText, t } from "./i18n";
import { patchSettings, useSettings } from "./settings";
import { Settings } from "./settingsSchema";
import {
  ACTIONS,
  ActionDef,
  ActionId,
  bindings,
  comboOf,
  comboText,
  conflicts,
  CONTEXTS,
  formatCombo,
  presetKeys,
  setRecording,
} from "./keymap";
import { IconHistory, IconX } from "./icons";

/// The actions the search query matches, for the dialog's count and list.
export function matchingActions(q: string): ActionDef[] {
  return ACTIONS.filter((a) => q === "" || searchText(a.label).includes(q) || a.id.includes(q));
}

export default function ShortcutsSection({ query, onError }: { query: string; onError: (p: Promise<void>) => void }) {
  const shortcuts = useSettings((s) => s.settings.shortcuts);
  const shown = matchingActions(query);
  const clash = conflicts();
  const inForce = bindings();
  const save = (overrides: Settings["shortcuts"]["overrides"]) => onError(patchSettings({ "shortcuts.overrides": overrides }));
  return (
    <>
      {CONTEXTS.map((ctx) => {
        const list = shown.filter((a) => a.context === ctx.id);
        if (list.length === 0) return null;
        return (
          <div key={ctx.id} className="shortcuts-context">
            <div className="shortcuts-context-head">{t(ctx.label)}</div>
            {list.map((a) => {
              const id = a.id as ActionId;
              const override = shortcuts.overrides[id];
              const combos = inForce.get(id) ?? [];
              const preset = presetKeys(id, shortcuts);
              return (
                <ShortcutRow
                  key={id}
                  action={a}
                  combos={combos}
                  overridden={override !== undefined}
                  presetText={preset.map(formatCombo).join(" / ")}
                  conflict={(clash.get(id) ?? []).map((o) => t(ACTIONS.find((x) => x.id === o)!.label))}
                  onRecord={(combo) => save({ ...shortcuts.overrides, [id]: combo })}
                  onReset={() => {
                    const { [id]: _, ...rest } = shortcuts.overrides;
                    save(rest);
                  }}
                />
              );
            })}
          </div>
        );
      })}
    </>
  );
}

function ShortcutRow({
  action,
  combos,
  overridden,
  presetText,
  conflict,
  onRecord,
  onReset,
}: {
  action: ActionDef;
  combos: readonly string[];
  overridden: boolean;
  presetText: string;
  conflict: string[];
  onRecord: (combo: string) => void;
  onReset: () => void;
}) {
  const [recording, setRec] = useState(false);
  useEffect(() => {
    setRecording(recording);
    return () => setRecording(false);
  }, [recording]);
  const stop = () => setRec(false);
  const onKeyDown = (e: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return stop();
    const c = comboOf(e.nativeEvent);
    if (!c) return; // a modifier alone: wait for the key
    stop();
    onRecord(comboText(c));
  };
  const shownCombo = combos.map(formatCombo).join(" / ");
  return (
    <div
      className={"setting-row shortcut-row" + (overridden ? " modified" : "")}
      data-key={`shortcuts.${action.id}`}
      title={overridden ? t("Changed from the default") : undefined}
    >
      <div className="setting-label">
        <span>{t(action.label)}</span>
        {conflict.length > 0 && (
          <span className="setting-default setting-error">{t("Also used by: {actions}", { actions: conflict.join(", ") })}</span>
        )}
        {overridden && <span className="setting-default">{t("Preset: {value}", { value: presetText || t("No shortcut") })}</span>}
      </div>
      <span className="shortcut-keys">
        <button
          className={"btn small shortcut-key" + (recording ? " recording" : "") + (combos.length === 0 && !recording ? " empty" : "")}
          title={t("Click, then press the keys. Esc cancels.")}
          onClick={() => setRec(true)}
          onKeyDown={onKeyDown}
          onBlur={stop}
        >
          {recording ? t("Press the keys…") : shownCombo || t("No shortcut")}
        </button>
        <button className="btn small" title={t("Remove the shortcut")} disabled={combos.length === 0} onClick={() => onRecord("")}>
          <IconX />
        </button>
        {overridden && (
          <button className="btn small" title={t("Back to the preset's shortcut")} onClick={onReset}>
            <IconHistory size={13} />
          </button>
        )}
      </span>
    </div>
  );
}
