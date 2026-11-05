<!-- Compact settings inside the flyout. Every change applies at once and is saved. -->
<script lang="ts">
  import type { Encoder } from "./bindings/Encoder";
  import type { Preset } from "./bindings/Preset";
  import AppLogo from "./AppLogo.svelte";
  import { bytes } from "./format";
  import Icon from "./Icon.svelte";
  import { CUSTOM_ID, logoFor, note, shortName } from "./presets";
  import Seg from "./Seg.svelte";
  import { changed, resolved, settings } from "./settings.svelte";
  import * as api from "./api";

  const OUTPUTS: { v: api.Settings["output"]; label: string; small: string }[] = [
    { v: "app", label: "Kompacter folder", small: "Next to the app (or Videos) · deleted after 7 days" },
    { v: "beside", label: "Next to the original", small: "In a “Kompacted” folder beside it" },
    { v: "custom", label: "A folder I choose", small: "" },
    { v: "replace", label: "Replace the original", small: "The original goes to the Recycle Bin" },
  ];
  async function chooseFolder() {
    const dir = await api.pickFolder().catch(() => null);
    if (dir) {
      settings.outputDir = dir;
      settings.output = "custom";
      changed();
    }
  }

  let { presets, encoders, onback }: { presets: Preset[]; encoders: Encoder[] | null; onback: () => void } = $props();

  const MB = 1024 * 1024;
  const ENC: Record<Encoder, string> = { nvenc: "NVIDIA NVENC", qsv: "Intel Quick Sync", amf: "AMD AMF", videotoolbox: "VideoToolbox" };
  const encText = $derived(
    encoders === null ? "Checking your GPU…"
    : encoders.length ? `${encoders.map((e) => ENC[e]).join(", ")} found · much faster`
    : "No GPU encoder found — using the CPU",
  );

  function setCustom(e: Event) {
    const mb = Math.max(1, Math.min(4096, Number((e.target as HTMLInputElement).value) || 10));
    settings.customBytes = Math.round(mb * MB);
    changed();
  }
  const set = <K extends keyof typeof settings>(k: K, v: (typeof settings)[K]) => {
    settings[k] = v;
    changed();
  };
</script>

{#snippet toggle(id: string, key: "gpu" | "pasteGuard" | "startWithWindows" | "autoHide", label: string, small = "", soon = false, describedby = "")}
  <div class="cell">
    <div class="grow" id={id}>{label}{#if soon}<span class="soon">Coming soon</span>{/if}{#if small}<small>{small}</small>{/if}</div>
    <span class="sw-state" aria-hidden="true">{settings[key] ? "On" : "Off"}</span>
    <button class="switch" role="switch" aria-checked={settings[key]} aria-labelledby={id} aria-describedby={describedby || undefined} onclick={() => set(key, !settings[key])}><span class="knob"></span></button>
  </div>
{/snippet}

<section class="view" aria-labelledby="setTitle">
  <div class="set-head">
    <button class="icon-btn press" aria-label="Back" onclick={onback}><Icon name="back" /></button>
    <h2 class="h1" id="setTitle">Settings</h2>
  </div>

  <h3 class="grp-h" id="g-target">Default target</h3>
  <div class="grp" role="radiogroup" aria-labelledby="g-target">
    {#each presets as p (p.id)}
      <label class="cell">
        <input type="radio" name="preset" value={p.id} checked={settings.preset === p.id} onchange={() => set("preset", p.id)} />
        <AppLogo logo={logoFor(p)} />
        <span class="grow">{shortName(p)}{#if note(p)}<small>{note(p)}</small>{/if}</span>
        {#if p.id === CUSTOM_ID}
          <input class="num" type="number" min="1" max="4096" value={Math.round(settings.customBytes / MB)} aria-label="Custom limit in MB" onchange={setCustom} /><span class="val">MB</span>
        {:else}
          <span class="val">{bytes(p.bytes)}</span>
        {/if}
        <span class="chk"><Icon name="check" /></span>
      </label>
    {/each}
  </div>

  <h3 class="grp-h" id="g-style">Accent / style</h3>
  <Seg label="Accent / style" describedby="styleNote" value={settings.style} onchange={(v) => set("style", v)}
    options={[{ v: "kompacter", label: "Kompacter" }, { v: "discord", label: "Discord" }]} />
  <p class="explain" id="styleNote">Auto-matches Discord’s theme when we can detect it.</p>

  <h3 class="grp-h" id="g-theme">Appearance</h3>
  {#if settings.style === "discord"}
    <Seg label="Discord theme" disabled={settings.discordTheme === "match"}
      value={resolved.dt}
      onchange={(v) => set("discordTheme", v)}
      options={[{ v: "light", label: "Light" }, { v: "ash", label: "Ash" }, { v: "dark", label: "Dark" }, { v: "onyx", label: "Onyx" }]} />
    <div class="grp gap">
      <div class="cell">
        <div class="grow" id="l-match">Match Windows<small>Light ↔ Dark with your Windows setting</small></div>
        <span class="sw-state" aria-hidden="true">{settings.discordTheme === "match" ? "On" : "Off"}</span>
        <button class="switch" role="switch" aria-checked={settings.discordTheme === "match"} aria-labelledby="l-match"
          onclick={() => set("discordTheme", settings.discordTheme === "match" ? resolved.dt : "match")}><span class="knob"></span></button>
      </div>
    </div>
  {:else}
    <Seg label="Appearance" value={settings.theme} onchange={(v) => set("theme", v)}
      options={[{ v: "system", label: "System" }, { v: "light", label: "Light" }, { v: "dark", label: "Dark" }]} />
  {/if}

  <h3 class="grp-h" id="g-out">Save compressed files to</h3>
  <div class="grp" role="radiogroup" aria-labelledby="g-out">
    {#each OUTPUTS as o (o.v)}
      <label class="cell">
        <input type="radio" name="output" value={o.v} checked={settings.output === o.v}
          onchange={() => (o.v === "custom" && !settings.outputDir ? chooseFolder() : set("output", o.v))} />
        <span class="grow">{o.label}<small>{o.v === "custom" ? settings.outputDir || "Pick a folder" : o.small}</small></span>
        {#if o.v === "custom"}
          <button class="btn plain press" onclick={(e) => (e.preventDefault(), chooseFolder())}>Choose…</button>
        {:else if o.v === "app"}
          <button class="btn plain press" onclick={(e) => (e.preventDefault(), api.openOutputs())}>Open</button>
        {/if}
        <span class="chk"><Icon name="check" /></span>
      </label>
    {/each}
  </div>

  <h3 class="grp-h">Encoding</h3>
  <div class="grp">{@render toggle("l-gpu", "gpu", "GPU encoding", encText)}</div>

  <h3 class="grp-h">General</h3>
  <div class="grp">
    {@render toggle("l-hide", "autoHide", "Hide when I click away", "Stays open while you drag a file to it")}
    {@render toggle("l-guard", "pasteGuard", "Catch oversized pastes", "", true, "guardExplain")}
    {@render toggle("l-start", "startWithWindows", "Start with Windows", "", true)}
    <div class="cell">
      <span class="grow" id="l-hot">Open Kompacter<span class="soon">Coming soon</span></span>
      <span class="keycap">{settings.hotkey.replaceAll("+", " + ")}</span>
    </div>
  </div>
  <p class="explain" id="guardExplain">Paste guard will only notice Ctrl+V while a chat app is in front, and only check the file’s size. Nothing leaves your PC.</p>

  <p class="privacy"><Icon name="lock" small />Everything runs on this PC. No account, no uploads.</p>
</section>

<style>
  .set-head { display: flex; align-items: center; gap: 4px; margin: 0 0 6px -8px; }
  .grp-h { margin: 16px 4px 6px; font-size: 12px; font-weight: 600; color: var(--label-2); text-transform: uppercase; letter-spacing: 0.03em; }
  .grp { border-radius: var(--r-md); background: var(--group); overflow: hidden; }
  .grp.gap { margin-top: 8px; }
  .cell { position: relative; display: flex; align-items: center; gap: 10px; min-height: 44px; padding: 8px 12px; font-size: 14px; }
  .cell + .cell::before { content: ""; position: absolute; left: 12px; right: 0; top: 0; border-top: 1px solid var(--sep); }
  .grow { flex: 1; min-width: 0; }
  small { display: block; font-size: 12px; color: var(--label-2); line-height: 16px; }
  .val { color: var(--label-2); font-variant-numeric: tabular-nums; }
  label.cell { cursor: pointer; }
  label.cell input[type="radio"] { position: absolute; opacity: 0; pointer-events: none; }
  .chk { width: 18px; color: var(--accent-text); opacity: 0; }
  label.cell input:checked ~ .chk { opacity: 1; }
  label.cell:has(input[type="radio"]:focus-visible) { outline: 3px solid var(--focus); outline-offset: -3px; border-radius: var(--r-md); }
  .num { width: 64px; height: 28px; border: 0; border-radius: 7px; background: var(--fill); padding: 0 8px; text-align: right; }
  .explain { margin: 6px 12px 0; font-size: 12px; line-height: 16px; color: var(--label-2); }
  .soon { margin-left: 8px; padding: 1px 7px; border-radius: 999px; background: var(--fill); color: var(--label-2); font-size: 11px; font-weight: 600; vertical-align: 1px; }
  .keycap { min-height: 28px; padding: 4px 10px; border-radius: 7px; background: var(--fill); font-size: 13px; font-weight: 500; color: var(--label-2); }
  .switch { position: relative; flex: none; width: 44px; height: 26px; border: 0; border-radius: 13px; background: var(--fill-2); padding: 0; transition: background-color 180ms var(--ease); }
  .knob { position: absolute; top: 2px; left: 2px; width: 22px; height: 22px; border-radius: 50%; background: #fff; box-shadow: 0 2px 4px rgba(0, 0, 0, 0.2), 0 0 0 0.5px rgba(0, 0, 0, 0.05); transition: transform 220ms var(--spring); }
  .switch[aria-checked="true"] { background: var(--accent); }
  .switch[aria-checked="true"] .knob { transform: translateX(18px); }
  .sw-state { font-size: 12px; color: var(--label-2); min-width: 22px; text-align: right; }
  .privacy { display: flex; gap: 8px; align-items: center; margin: 16px 4px 0; font-size: 12px; color: var(--label-2); }
</style>
