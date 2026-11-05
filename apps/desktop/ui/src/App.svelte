<!--
  The flyout. One calm base size (360×472); only the trimmer and settings grow taller.
  The card grows open from its bottom edge and folds back down (lib/window.ts).
  A small state machine decides what the stage and the view below it show.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import * as api from "./lib/api";
  import type { Encoder } from "./lib/bindings/Encoder";
  import type { Preset } from "./lib/bindings/Preset";
  import DoneView from "./lib/DoneView.svelte";
  import FileCard from "./lib/FileCard.svelte";
  import { bytes, fileName } from "./lib/format";
  import Icon from "./lib/Icon.svelte";
  import { live, say } from "./lib/live.svelte";
  import PresetPill from "./lib/PresetPill.svelte";
  import { withCustom } from "./lib/presets";
  import ProgressView from "./lib/ProgressView.svelte";
  import QueueView from "./lib/QueueView.svelte";
  import SettingsView from "./lib/SettingsView.svelte";
  import { changed, loadSettings, settings } from "./lib/settings.svelte";
  import SizeMeter from "./lib/SizeMeter.svelte";
  import Stage, { type Mood, type StageSize } from "./lib/Stage.svelte";
  import Trimmer, { type TrimRequest } from "./lib/Trimmer.svelte";
  import { BASE_H, card } from "./lib/window";

  type From = "idle" | "big" | "trim" | "queue";
  type View =
    | { k: "idle" }
    | { k: "reading"; path: string }
    | { k: "big"; path: string; ins: api.Inspection }
    | { k: "queue"; path: string; ins: api.Inspection }
    | { k: "trim"; path: string; ins: api.Inspection }
    | { k: "busy"; path: string; ins: api.Inspection; label: string; from: From }
    | { k: "done"; result: api.Compressed; label: string }
    | { k: "error"; path: string; message: string }
    | { k: "settings"; back: View };

  let enginePresets = $state<Preset[]>([]);
  let encoders = $state<Encoder[] | null>(null);
  let view = $state<View>({ k: "idle" });
  let dragging = $state(false);
  let progress = $state(0);
  let cancelling = false;
  let stage: Stage;
  let flyout: HTMLDivElement;

  const presets = $derived(withCustom(enginePresets, settings.customBytes));
  const preset = $derived(presets.find((p) => p.id === settings.preset) ?? presets[0] ?? { id: "discord", label: "Discord", bytes: 20 * 1024 * 1024 });
  const target = $derived(preset.bytes);

  const STAGE: Partial<Record<View["k"], StageSize>> = { idle: "lg", reading: "md", big: "md", queue: "md", busy: "press", done: "lg", error: "md" };
  const stageSize = $derived(STAGE[view.k]);
  const mood: Mood = $derived(
    dragging ? "hungry"
    : view.k === "big" || view.k === "error" ? "worried"
    : view.k === "busy" || view.k === "reading" ? "busy"
    : view.k === "done" ? (view.result.fits ? "happy" : "worried")
    : "idle",
  );

  /* ── Card height: base for everything except the trimmer and settings ── */
  const HEIGHT: Partial<Record<View["k"], number>> = { trim: 608, settings: 600 };
  $effect(() => {
    card.setTarget(HEIGHT[view.k] ?? BASE_H);
  });

  /* ── Open / close / auto-hide ── */
  let hideTimer = 0;
  let hovering = false;
  async function close() {
    clearTimeout(hideTimer);
    await card.close();
    await api.hide().catch(() => {});
  }
  /** Fold away after `ms` if the user has moved on (not hovering, not dragging, not focused). */
  function autoHide(ms: number) {
    clearTimeout(hideTimer);
    if (!settings.autoHide) return;
    hideTimer = window.setTimeout(() => {
      if (!hovering && !dragging && !document.hasFocus() && view.k !== "busy") close();
    }, ms);
  }

  /* ── Flow ── */
  const media = (ins: api.Inspection) => {
    const { info } = ins;
    const parts: string[] = [];
    if (info.duration > 0) parts.push(`${Math.floor(info.duration / 60)}:${String(Math.floor(info.duration % 60)).padStart(2, "0")}`);
    if (info.video) parts.push(`${info.video.width}×${info.video.height}`);
    parts.push(bytes(info.size_bytes));
    return parts.join(" · ");
  };

  async function open(path: string) {
    if (view.k === "busy") return;
    view = { k: "reading", path };
    try {
      const ins = await api.inspect(path, target);
      if (ins.info.kind === "folder") view = { k: "queue", path, ins };
      else if (ins.estimate.fits_as_is || ins.info.kind !== "video") compress(path, ins, {}, false, "", "idle");
      else view = { k: "big", path, ins };
    } catch (e) {
      view = { k: "error", path, message: String(e) };
      say(`Couldn't read ${fileName(path)}: ${e}`);
    }
  }

  async function compress(path: string, ins: api.Inspection, range: api.Range, keepQuality: boolean, label: string, from: From) {
    const plan = ins.estimate.video;
    label ||= keepQuality ? "Original quality" : plan ? `${plan.height}p` : ins.estimate.audio_kbps ? `${ins.estimate.audio_kbps} kbps` : "";
    progress = 0;
    cancelling = false;
    const quick = ins.estimate.fits_as_is && !range.start && !range.end;
    if (!quick) view = { k: "busy", path, ins, label, from };
    try {
      const result = await api.compress(path, target, range, keepQuality);
      progress = 1;
      if (!quick) await stage.finish();
      view = { k: "done", result, label: result.untouched ? "" : label };
      stage.celebrate();
      if (!document.hasFocus()) autoHide(2500);
      say(result.fits ? (result.copied ? `Copied ${fileName(result.path)}, ${bytes(result.output_bytes)}. Paste anywhere with Control V.` : `Saved ${bytes(result.output_bytes)}.`) : `Still too big: ${bytes(result.output_bytes)}.`);
    } catch (e) {
      if (cancelling) {
        say("Compression cancelled");
        view = from === "trim" ? { k: "trim", path, ins } : from === "queue" ? { k: "queue", path, ins } : from === "big" ? { k: "big", path, ins } : { k: "idle" };
      } else {
        view = { k: "error", path, message: String(e) };
        say(`Compression failed: ${e}`);
      }
    }
  }

  function cancel() {
    cancelling = true;
    api.cancel();
  }

  async function choose() {
    const p = await api.pickFile().catch(() => null);
    if (p) open(p);
  }
  async function paste() {
    const files = await api.clipboardFiles().catch(() => [] as string[]);
    if (files.length) open(files[0]);
    else say("No copied file on the clipboard. Copy one in Explorer first.");
  }

  function selectPreset(id: string) {
    settings.preset = id;
    changed();
    say(`Target set to ${bytes(presets.find((p) => p.id === id)?.bytes ?? 0)}`);
    if (view.k === "big" || view.k === "queue") open(view.path); // re-estimate for the new limit
  }
  function goSettings() {
    if (view.k === "settings") view = view.back;
    else if (view.k !== "busy") view = { k: "settings", back: view };
  }

  function onKey(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    if (e.key === "Escape") {
      if (view.k === "settings") view = view.back;
      else if (view.k === "trim") view = { k: "big", path: view.path, ins: view.ins };
      else close();
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v" && !(e.target instanceof HTMLInputElement)) {
      e.preventDefault();
      paste();
    }
  }

  onMount(() => {
    loadSettings();
    api.presets().then((p) => (enginePresets = p)).catch(() => {});
    api.encoders().then((e) => (encoders = e)).catch(() => (encoders = []));

    card.attach(flyout);
    const unOpen = listen("flyout-open", () => {
      clearTimeout(hideTimer);
      card.open();
    }).catch(() => () => {});
    const unClose = listen("flyout-close", () => close()).catch(() => () => {});
    const onBlur = () => autoHide(1500);
    const onFocus = () => clearTimeout(hideTimer);
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);

    const onVis = () => (document.hidden ? (document.documentElement.dataset.hidden = "") : delete document.documentElement.dataset.hidden);
    document.addEventListener("visibilitychange", onVis);

    const unProgress = api.onProgress((p) => (progress = p)).catch(() => () => {});
    const unDrop = getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        if (view.k === "busy") return;
        if (payload.type === "enter" || payload.type === "over") {
          clearTimeout(hideTimer);
          if (!dragging && view.k !== "idle") view = { k: "idle" };
          dragging = true;
          const r = devicePixelRatio || 1;
          stage?.look(payload.position.x / r, payload.position.y / r);
        } else if (payload.type === "leave") dragging = false;
        else if (payload.type === "drop") {
          dragging = false;
          if (payload.paths.length) open(payload.paths[0]);
        }
      })
      .catch(() => () => {});

    return () => {
      unOpen.then((f) => f());
      unClose.then((f) => f());
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVis);
      unProgress.then((f) => f());
      unDrop.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={onKey} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="flyout" data-view={view.k} bind:this={flyout}
  onpointerenter={() => ((hovering = true), clearTimeout(hideTimer))}
  onpointerleave={() => ((hovering = false), document.hasFocus() || autoHide(1500))}>
  <div class="fly-inner">
    <header class="fly-head">
      <PresetPill {presets} selected={preset} onselect={selectPreset} onedit={goSettings} />
      <span class="spacer"></span>
      <button class="icon-btn press" aria-label="Settings" aria-pressed={view.k === "settings"} disabled={view.k === "busy"} onclick={goSettings}><Icon name="gear" /></button>
    </header>

    <div class="fly-body">
      <div class="stage-wrap" hidden={!stageSize}>
        <Stage bind:this={stage} size={stageSize ?? "lg"} {mood} {dragging} {progress} showDash={view.k === "idle"} />
      </div>

      {#key view.k}
        <div class="view-anim">
          {#if view.k === "idle"}
            <section class="view center" aria-labelledby="idleTitle">
              <h2 class="h1" id="idleTitle">{dragging ? "Let go to Kompact it" : "Drop or copy a file"}</h2>
              <p class="sub">It shrinks to fit <b>{bytes(target)}</b> and lands on your clipboard.</p>
              <div class="row c" style:margin-top="16px">
                <button class="btn primary press" onclick={choose}>Choose File…</button>
                <button class="btn press" onclick={paste}><Icon name="clip" small />Paste</button>
              </div>
              <p class="hint">or press <kbd>Ctrl</kbd><kbd>V</kbd> here with a file copied</p>
            </section>
          {:else if view.k === "reading"}
            <section class="view center">
              <h2 class="h1 ellipsis">{fileName(view.path)}</h2>
              <p class="sub">Reading…</p>
            </section>
          {:else if view.k === "big"}
            {@const v = view}
            {@const est = v.ins.estimate}
            <section class="view" aria-labelledby="bigName">
              <FileCard kind={v.ins.info.kind} name={fileName(v.path)} meta={media(v.ins)} onremove={() => (view = { k: "idle" })} />
              <SizeMeter size={v.ins.info.size_bytes} {target} />
              <div class="status bad"><Icon name="xc" small /><span>{bytes(v.ins.info.size_bytes - target)} over your {bytes(target)} limit</span></div>
              <div class="actions">
                {#if est.video && !est.video.fits}
                  <button class="btn primary lg press" onclick={() => (view = { k: "trim", path: v.path, ins: v.ins })}><Icon name="scissors" small />Trim</button>
                  <button class="btn lg press" onclick={() => compress(v.path, v.ins, {}, false, "", "big")}><Icon name="squeeze" small />Compress</button>
                {:else}
                  <button class="btn primary lg press" onclick={() => compress(v.path, v.ins, {}, false, "", "big")}><Icon name="squeeze" small />Compress</button>
                  <button class="btn lg press" disabled={v.ins.info.duration <= 0} onclick={() => (view = { k: "trim", path: v.path, ins: v.ins })}><Icon name="scissors" small />Trim</button>
                {/if}
              </div>
              <p class="foot-note">
                {#if est.video}
                  {est.video.fits ? `Compress makes it fit at ${est.video.height}p · ${est.video.quality} quality` : "✗ Too long to fit at a watchable quality — trim it first"}
                {/if}
              </p>
            </section>
          {:else if view.k === "queue"}
            {@const v = view}
            <QueueView info={v.ins.info} {target} oncompress={() => compress(v.path, v.ins, {}, false, "zip", "queue")} onremove={() => (view = { k: "idle" })} />
          {:else if view.k === "trim"}
            {@const v = view}
            <Trimmer path={v.path} info={v.ins.info} {target} onback={() => (view = { k: "big", path: v.path, ins: v.ins })}
              oncompress={(r: TrimRequest) => compress(v.path, v.ins, { start: r.start, end: r.end }, r.keepQuality, r.label, "trim")} />
          {:else if view.k === "busy"}
            {@const v = view}
            <ProgressView {progress} name={fileName(v.path)} oncancel={cancel}
              meta="{bytes(v.ins.info.size_bytes)} → ≤ {bytes(target)}{v.label ? ` · ${v.label}` : ''} · {settings.gpu && encoders?.length ? 'GPU' : 'CPU'}" />
          {:else if view.k === "done"}
            <DoneView result={view.result} label={view.label} onanother={() => (view = { k: "idle" })} />
          {:else if view.k === "error"}
            <section class="view center" aria-labelledby="errTitle">
              <h2 class="h1" id="errTitle"><span class="bad" aria-hidden="true">✗</span> Couldn't do that</h2>
              <p class="sub">{fileName(view.path)}: {view.message}</p>
              <div class="row c" style:margin-top="16px"><button class="btn primary press" onclick={() => (view = { k: "idle" })}>Try another file</button></div>
            </section>
          {:else if view.k === "settings"}
            <SettingsView presets={presets} {encoders} onback={goSettings} />
          {/if}
        </div>
      {/key}
    </div>
  </div>
</div>

<div class="sr-only" aria-live="polite">{live.text}</div>

<style>
  /* The card: anchored to the window's bottom edge; only its height animates (lib/window.ts).
     It draws its own shadow and hairline border, since the OS window shadow is off. */
  .flyout {
    position: absolute; left: 12px; bottom: 12px; width: 360px; height: 472px;
    overflow: hidden; border-radius: var(--r-xl);
    background: var(--mat-strong);
    box-shadow: 0 0 0 0.5px var(--hair), 0 8px 24px -6px rgba(0, 0, 0, 0.32);
    contain: layout paint;
  }
  :global(.flyout.closed) { visibility: hidden; }
  /* Content keeps its full height and sits on the bottom edge, so a shorter card
     reveals it from the bottom up instead of squashing it. */
  .fly-inner { position: absolute; left: 0; right: 0; bottom: 0; height: var(--content-h, 472px); display: flex; flex-direction: column; }
  .fly-head { display: flex; align-items: center; gap: 8px; padding: 14px 14px 0 16px; position: relative; z-index: 2; }
  .spacer { flex: 1; }
  .fly-body { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; padding: 6px 20px 20px; overflow: auto; overscroll-behavior: contain; scrollbar-width: thin; }
  .stage-wrap { flex: 1 0 auto; display: flex; flex-direction: column; }
  [data-view="trim"] .stage-wrap, [data-view="settings"] .stage-wrap { display: none; }
  .view-anim { flex: none; animation: viewIn 220ms var(--ease) both; }
  @keyframes viewIn { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: none; } }
  .ellipsis { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
</style>
