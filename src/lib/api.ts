import { invoke } from "@tauri-apps/api/core";

export type Kind = "text" | "image" | "files";

export interface Clip {
  id: number;
  kind: Kind;
  preview: string;
  sourceApp: string | null;
  createdAt: number;
  usedAt: number;
  pinned: boolean;
  size: number;
  hasHtml: boolean;
  hasRtf: boolean;
  imageWidth: number | null;
  imageHeight: number | null;
  thumbUrl: string | null;
  imageUrl: string | null;
}

export interface Detail {
  id: number;
  kind: Kind;
  text: string;
  truncated: boolean;
  files: string[];
}

export interface Settings {
  hotkey: string;
  maxItems: number;
  maxImageMb: number;
  maxItemMb: number;
  paused: boolean;
  autostart: boolean;
  ignoredApps: string[];
  welcomeDone: boolean;
}

export interface SettingsView {
  settings: Settings;
  hotkeyError: string | null;
  stats: { items: number; pinned: number; imageBytes: number; dataDir: string };
  version: string;
}

export interface PopupShown {
  welcome: boolean;
  settings: boolean;
  canPaste: boolean;
}

export interface UpdateStatus {
  phase: "idle" | "checking" | "upToDate" | "downloading" | "ready" | "error";
  version: string | null;
  notes: string | null;
  message: string | null;
}

export const api = {
  updateStatus: () => invoke<UpdateStatus>("update_status"),
  checkUpdate: () => invoke<void>("check_update"),
  /** Exits the app; the installer relaunches the new version. */
  installUpdate: () => invoke<void>("install_update"),
  search: (query: string, offset: number, limit: number) =>
    invoke<Clip[]>("search", { query, offset, limit }),
  detail: (id: number) => invoke<Detail>("detail", { id }),
  /** Copy to clipboard, hide popup and (if `paste`) paste into the previous app. */
  activate: (id: number, plain: boolean, paste: boolean) =>
    invoke<void>("activate", { id, plain, paste }),
  deleteItem: (id: number) => invoke<void>("delete_item", { id }),
  togglePin: (id: number) => invoke<boolean>("toggle_pin", { id }),
  clearHistory: (keepPinned: boolean) => invoke<number>("clear_history", { keepPinned }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (settings: Settings) => invoke<SettingsView>("save_settings", { settings }),
  suspendHotkey: (suspend: boolean) => invoke<void>("suspend_hotkey", { suspend }),
  hide: () => invoke<void>("hide_popup"),
  openDataDir: () => invoke<void>("open_data_dir"),
};

export function timeAgo(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return "now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.round(h / 24);
  if (d < 30) return `${d}d`;
  return new Date(ms).toLocaleDateString();
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Turns a keydown event into a shortcut string like "Ctrl+Alt+V". */
export function shortcutFromEvent(e: KeyboardEvent): string | null {
  const modifierCodes = ["Control", "Alt", "Shift", "Meta", "OS", "AltGraph"];
  if (modifierCodes.some((m) => e.key === m || e.code.startsWith(m))) return null;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  if (mods.length === 0) return null;
  let key = e.code;
  if (/^Key[A-Z]$/.test(key)) key = key.slice(3);
  else if (/^Digit\d$/.test(key)) key = key.slice(5);
  return [...mods, key].join("+");
}
