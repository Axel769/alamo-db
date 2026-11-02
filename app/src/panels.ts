// Sidebar panels contributed through a registry (architecture §13): the
// activity bar shows one icon for each, after the built-in panels that need
// the Studio's state (tree, connections). An extension build registers its
// own before the app renders (`main.tsx` renders when imported).
import type { ComponentType } from "react";
import HistoryPanel from "./HistoryPanel";
import { t } from "./i18n";
import { IconHistory } from "./icons";

export type SidePanelProps = {
  /// The user picked something (an entry, a table): a floating panel closes.
  done: () => void;
};

export type SidePanel = {
  /// Stable: remembered as the last open panel.
  id: string;
  /// Tooltip of the icon, in the current language.
  title: () => string;
  icon: ComponentType;
  component: ComponentType<SidePanelProps>;
};

const panels: SidePanel[] = [
  { id: "history", title: () => t("Query history"), icon: IconHistory, component: HistoryPanel },
];

/// Adds `panel`, replacing the one with the same id.
export function registerPanel(panel: SidePanel): void {
  const i = panels.findIndex((p) => p.id === panel.id);
  if (i >= 0) panels[i] = panel;
  else panels.push(panel);
}

export const sidePanels = (): readonly SidePanel[] => panels;
