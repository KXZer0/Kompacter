// Typed wrappers around the Rust commands. Types in ./bindings are generated from the
// engine (`npm run types`), so a renamed field breaks the build instead of the app.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Estimate } from "./bindings/Estimate";
import type { MediaInfo } from "./bindings/MediaInfo";
import type { Outcome } from "./bindings/Outcome";
import type { Preset } from "./bindings/Preset";

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
