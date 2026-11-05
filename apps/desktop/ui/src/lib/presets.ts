// How presets from the engine are shown: short names and which app glyph labels them.
import type { Preset } from "./bindings/Preset";
import type { Brand } from "./logos";

export type Logo = Brand | "mail" | "custom";

export const CUSTOM_ID = "custom";

/** "Discord · 20 MB" style short name. Falls back to the engine's label. */
export function shortName(p: Preset): string {
  const id = p.id.toLowerCase();
  if (id === CUSTOM_ID) return "Custom";
  if (id === "discord-nitro-basic") return "Nitro Basic";
  if (id === "discord-nitro") return "Nitro";
  if (id === "discord") return "Discord";
  if (id.startsWith("email") || id.startsWith("mail")) return "Email";
  return p.label.replace(/\s*\(.*\)$/, "");
}

/** Second line in lists, e.g. "Free" for "Discord (free)". */
export function note(p: Preset): string {
  const m = p.label.match(/\((.*)\)$/);
  if (m) return m[1][0].toUpperCase() + m[1].slice(1);
  return p.id.startsWith("discord-") ? "Discord" : "";
}

export function logoFor(p: Preset): Logo {
  const id = p.id.toLowerCase();
  if (id.startsWith("discord")) return "discord";
  if (id.startsWith("whatsapp")) return "whatsapp";
  if (id.startsWith("telegram")) return "telegram";
  if (id.startsWith("slack")) return "slack";
  if (id.startsWith("gmail")) return "gmail";
  if (id === CUSTOM_ID) return "custom";
  return "mail";
}

/** The engine's presets plus a "Custom" entry sized from settings. */
export const withCustom = (list: Preset[], customBytes: number): Preset[] => [
  ...list,
  { id: CUSTOM_ID, label: "Custom", bytes: customBytes },
];
