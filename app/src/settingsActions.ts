// Actions of the settings window (§16): buttons that live in a section
// next to the values, found by the search like a setting. Each one runs
// and answers with a line for the row, or throws the backend's message.
import { ipc } from "./ipc";
import { t } from "./i18n";
import type { Update } from "./UpdateDialog";

export type SettingAction = {
  id: string;
  category: string;
  label: string;
  help: string;
  /// The line shown next to the button afterwards; nothing = no line.
  run: () => Promise<string | void>;
};

export const SETTING_ACTIONS: SettingAction[] = [
  {
    id: "updates.check_now",
    category: "updates",
    label: "Check now",
    help: "Asks for the latest published version; a newer one opens the update window.",
    run: async () => {
      const u = await ipc.updateCheck();
      if (!u) return t("You're up to date.");
      window.dispatchEvent(new CustomEvent<Update>("alamo:update-found", { detail: u }));
      return t("Update available: {v}", { v: u.version });
    },
  },
  {
    id: "updates.copy_diagnostics",
    category: "updates",
    label: "Copy diagnostics",
    help: "Version, system, engines and changed settings, for a bug report: nothing about hosts, profiles or passwords.",
    run: async () => {
      await navigator.clipboard.writeText(await ipc.diagnosticsText());
      return t("Copied.");
    },
  },
  {
    id: "updates.reveal",
    category: "updates",
    label: "Open settings folder",
    help: "Shows settings.json in the file manager.",
    run: async () => {
      await ipc.settingsReveal();
    },
  },
];
