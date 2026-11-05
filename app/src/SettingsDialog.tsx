// Settings window (§16): categories on the left, the settings of the chosen
// one on the right, a search that finds a setting by its name in either
// language. Every control is drawn from `SETTING_DEFS` by `kind`, so a new
// setting declared in settings.rs appears here without a line of UI code.
// Changes apply at once (the dialog itself switches language with the
// rest of the app); a value that differs from the default is marked and
// says what the default is; "Restore defaults" removes a section's values
// from settings.json. Values the file had and this build could not take
// are listed at the top, once.
import { useEffect, useRef, useState } from "react";
import { searchText, t, tMsg } from "./i18n";
import { patchSettings, resetSettings, settingOf, useSettings } from "./settings";
import { DEFAULTS, SETTING_CATEGORIES, SETTING_DEFS, SettingDef, SettingValues } from "./settingsSchema";
import { IconSearch, IconX } from "./icons";

export default function SettingsDialog({ onClose }: { onClose: () => void }) {
  const settings = useSettings((s) => s.settings);
  const issues = useSettings((s) => s.issues);
  const [category, setCategory] = useState(SETTING_CATEGORIES[0].id);
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      // Nothing of the window focused: the Studio shortcuts must not act
      // behind it. Inside, the controls take their keys (Enter on a text
      // field) and the window's root stops them from reaching Studio.
      if (!(e.target instanceof Node && root.current?.contains(e.target))) e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const save = (p: Promise<void>) => {
    setError(null);
    p.catch((e) => setError(tMsg(String(e))));
  };
  const q = query.trim().toLowerCase();
  const matches = (d: SettingDef) =>
    q === "" ||
    searchText(d.label).includes(q) ||
    searchText(categoryLabel(d.category)).includes(q) ||
    d.key.includes(q);
  const found = SETTING_DEFS.filter(matches);
  // Without a query, one section; with a query, every category that has a
  // match, so the user sees where the setting lives.
  const shown = q === "" ? SETTING_CATEGORIES.filter((c) => c.id === category) : SETTING_CATEGORIES.filter((c) => found.some((d) => d.category === c.id));
  const isDefault = (d: SettingDef) => settingOf(settings, d.key) === settingOf(DEFAULTS, d.key);

  return (
    <div className="dialog-overlay">
      <div className="dialog settings-dialog" ref={root} onKeyDown={(e) => e.stopPropagation()}>
        <div className="dialog-title">
          <span>{t("Settings")}</span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <nav className="settings-nav">
            {SETTING_CATEGORIES.map((c) => {
              const n = q === "" ? 0 : found.filter((d) => d.category === c.id).length;
              return (
                <button
                  key={c.id}
                  className={"settings-nav-item" + (q === "" && c.id === category ? " active" : "") + (q !== "" && n === 0 ? " dim" : "")}
                  onClick={() => {
                    setQuery("");
                    setCategory(c.id);
                  }}
                >
                  <span>{t(c.label)}</span>
                  {q !== "" && n > 0 && <span className="settings-nav-count">{n}</span>}
                </button>
              );
            })}
          </nav>
          <div className="settings-main">
            <label className="settings-search">
              <IconSearch size={14} />
              <input
                type="search"
                autoFocus
                placeholder={t("Search settings")}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            {issues.length > 0 && (
              <div className="settings-issues">
                <div>{t("Some values in settings.json could not be used and are back to the default:")}</div>
                <ul>
                  {issues.map((i) => (
                    <li key={i.key}>
                      <code>{i.key}</code>
                      {i.found && (
                        <>
                          {" = "}
                          <code>{i.found}</code>
                        </>
                      )}
                      {" — "}
                      {tMsg(i.message)}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {error && <div className="app-update-error">{error}</div>}
            {shown.length === 0 && <div className="settings-empty">{t("No setting matches \"{q}\".", { q: query.trim() })}</div>}
            {shown.map((c) => {
              const defs = found.filter((d) => d.category === c.id);
              const allDefault = SETTING_DEFS.filter((d) => d.category === c.id).every(isDefault);
              return (
                <section key={c.id} className="settings-section">
                  <div className="settings-section-head">
                    <h3>{t(c.label)}</h3>
                    <button
                      className="btn small"
                      disabled={allDefault}
                      title={t("Back to the defaults of this section: the saved values are removed from settings.json.")}
                      onClick={() => save(resetSettings(c.id))}
                    >
                      {t("Restore defaults")}
                    </button>
                  </div>
                  {defs.map((d) => (
                    <SettingRow
                      key={d.key}
                      def={d}
                      value={settingOf(settings, d.key)}
                      modified={!isDefault(d)}
                      onChange={(v) => save(patchSettings({ [d.key]: v } as Partial<SettingValues>))}
                    />
                  ))}
                </section>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}

function categoryLabel(id: string): string {
  return SETTING_CATEGORIES.find((c) => c.id === id)?.label ?? id;
}

/// The default of a setting as the user reads it: the option's label for
/// an enum, the text itself otherwise.
function defaultText(def: SettingDef): string {
  const v = settingOf(DEFAULTS, def.key);
  if (def.kind.kind === "enum") {
    const i = def.kind.values.indexOf(v);
    return i >= 0 ? t(def.kind.labels[i]) : v;
  }
  return v === "" ? t("(empty)") : v;
}

function SettingRow({ def, value, modified, onChange }: { def: SettingDef; value: string; modified: boolean; onChange: (v: string) => void }) {
  return (
    <div className={"setting-row" + (modified ? " modified" : "")} data-key={def.key} title={modified ? t("Changed from the default") : undefined}>
      <div className="setting-label">
        <span>{t(def.label)}</span>
        {modified && <span className="setting-default">{t("Default: {value}", { value: defaultText(def) })}</span>}
      </div>
      <SettingControl def={def} value={value} onChange={onChange} />
    </div>
  );
}

function SettingControl({ def, value, onChange }: { def: SettingDef; value: string; onChange: (v: string) => void }) {
  switch (def.kind.kind) {
    case "enum": {
      const { values, labels } = def.kind;
      return (
        <select value={value} onChange={(e) => onChange(e.target.value)}>
          {values.map((v, i) => (
            <option key={v} value={v}>
              {t(labels[i])}
            </option>
          ))}
        </select>
      );
    }
    case "str":
      return <TextSetting value={value} onChange={onChange} />;
  }
}

/// Free text is written when the field is left or Enter is pressed, not
/// at every keystroke: each write goes to the disk.
function TextSetting({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const commit = () => {
    if (draft !== value) onChange(draft);
  };
  return (
    <input
      type="text"
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit();
      }}
    />
  );
}
