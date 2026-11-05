// User settings: loaded once, saved (debounced) on every change, and mirrored onto <html>
// as data attributes that themes.css keys off.
import * as api from "./api";

export const DEFAULTS: api.Settings = {
  preset: "discord",
  customBytes: 20 * 1024 * 1024,
  style: "kompacter",
  theme: "system",
  discordTheme: "match",
  gpu: true,
  pasteGuard: false,
  startWithWindows: false,
  hotkey: "Ctrl+Shift+K",
  output: "app",
  outputDir: "",
  autoHide: true,
};

export const settings = $state<api.Settings>({ ...DEFAULTS });
/** The Discord theme actually shown (resolves "match"). */
export const resolved = $state({ dt: "dark" as "light" | "ash" | "dark" | "onyx" });

const darkMq = matchMedia("(prefers-color-scheme: dark)");

/** Write the theme onto <html>. Discord "match" follows Windows light/dark. */
export function applyTheme() {
  const d = document.documentElement.dataset;
  if (settings.theme === "system") delete d.theme;
  else d.theme = settings.theme;
  if (settings.style === "discord") {
    d.style = "discord";
    resolved.dt = settings.discordTheme === "match" ? (darkMq.matches ? "dark" : "light") : settings.discordTheme;
    d.dt = resolved.dt;
  } else {
    delete d.style;
    delete d.dt;
  }
}
darkMq.addEventListener("change", applyTheme);

let saveTimer = 0;
/** Persist after changes settle; theme applies immediately. */
export function changed() {
  applyTheme();
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    api.setSettings($state.snapshot(settings)).catch(() => {});
  }, 300);
}

export async function loadSettings() {
  try {
    Object.assign(settings, await api.getSettings());
  } catch {
    /* running outside Tauri (vite dev in a browser): keep defaults */
  }
  applyTheme();
}
