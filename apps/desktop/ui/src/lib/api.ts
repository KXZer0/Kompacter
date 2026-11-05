// Typed wrappers around the Rust commands. Types in ./bindings are generated from the
// engine (`npm run types`), so a renamed field breaks the build instead of the app.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Estimate } from "./bindings/Estimate";
import type { MediaInfo } from "./bindings/MediaInfo";
import type { Outcome } from "./bindings/Outcome";
import type { Preset } from "./bindings/Preset";
import type { Encoder } from "./bindings/Encoder";

export type Inspection = { info: MediaInfo; estimate: Estimate };
export type Compressed = Outcome & { copied: boolean };

export interface Range {
  start?: number;
  end?: number;
}

export const presets = () => invoke<Preset[]>("presets");

export const inspect = (path: string, target: number, range: Range = {}) =>
  invoke<Inspection>("inspect", { path, target, ...range });

export const compress = (path: string, target: number, range: Range = {}, keepQuality = false) =>
  invoke<Compressed>("compress", { path, target, keepQuality, ...range });

export const cancel = () => invoke<void>("cancel");
export const hide = () => invoke<void>("hide");

/** Compression progress, 0..1. */
export const onProgress = (cb: (p: number) => void): Promise<UnlistenFn> =>
  listen<number>("progress", (e) => cb(e.payload));

/** Mirrors `Settings` in src-tauri/src/settings.rs (camelCase). */
export type Settings = {
  preset: string;
  customBytes: number;
  style: "kompacter" | "discord";
  theme: "system" | "light" | "dark";
  discordTheme: "match" | "light" | "ash" | "dark" | "onyx";
  gpu: boolean;
  pasteGuard: boolean;
  startWithWindows: boolean;
  hotkey: string;
};

/** Native open-file dialog; null if cancelled. */
export const pickFile = () => invoke<string | null>("pick_file");
/** Files copied in Explorer (Windows); empty if none. */
export const clipboardFiles = () => invoke<string[]>("clipboard_files");
export const showInFolder = (path: string) => invoke<void>("show_in_folder", { path });
/** Working GPU encoders; empty = CPU only. */
export const encoders = () => invoke<Encoder[]>("encoders");
export const getSettings = () => invoke<Settings>("get_settings");
export const setSettings = (settings: Settings) => invoke<void>("set_settings", { settings });
