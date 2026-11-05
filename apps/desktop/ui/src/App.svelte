<!-- The flyout. One fixed size; the state machine below decides what it shows. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import * as api from "./lib/api";
  import type { Preset } from "./lib/bindings/Preset";
  import { bytes, fileName } from "./lib/format";
  import Stage, { type Mood } from "./lib/Stage.svelte";

  type View =
    | { kind: "idle" }
    | { kind: "reading"; path: string }
    | { kind: "big"; path: string; found: api.Inspection }
    | { kind: "busy"; path: string }
    | { kind: "done"; result: api.Compressed }
    | { kind: "fits"; path: string; size: number }
    | { kind: "error"; path: string; message: string };

  let presets = $state<Preset[]>([]);
  let presetIndex = $state(0);
  let view = $state<View>({ kind: "idle" });
  let dragging = $state(false);
  let progress = $state(0);

  const target = $derived(presets[presetIndex]?.bytes ?? 20 * 1024 * 1024);

  const mood: Mood = $derived(
    dragging ? "over"
    : view.kind === "big" || view.kind === "error" ? "big"
    : view.kind === "busy" || view.kind === "reading" ? "busy"
    : view.kind === "done" || view.kind === "fits" ? "done"
    : "idle",
  );

  async function open(path: string) {
    view = { kind: "reading", path };
    progress = 0;
    try {
      const found = await api.inspect(path, target);
      view = found.estimate.fits_as_is
        ? { kind: "fits", path, size: found.info.size_bytes }
        : { kind: "big", path, found };
    } catch (e) {
      view = { kind: "error", path, message: String(e) };
    }
  }

  async function compress(path: string) {
    view = { kind: "busy", path };
    progress = 0;
    try {
      view = { kind: "done", result: await api.compress(path, target) };
    } catch (e) {
      view = { kind: "error", path, message: String(e) };
    }
  }

  function reopenCurrent() {
    if (view.kind === "big" || view.kind === "fits" || view.kind === "error") open(view.path);
  }

  onMount(() => {
    api.presets().then((p) => (presets = p));
    const unlisten = [
      api.onProgress((p) => (progress = p)),
      getCurrentWebview().onDragDropEvent(({ payload }) => {
        if (view.kind === "busy" || view.kind === "reading") return;
        if (payload.type === "enter" || payload.type === "over") dragging = true;
        else if (payload.type === "leave") dragging = false;
        else if (payload.type === "drop") {
          dragging = false;
          if (payload.paths.length) open(payload.paths[0]);
        }
      }),
    ];
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && api.hide();
    window.addEventListener("keydown", onKey);
    return () => {
      unlisten.forEach((u) => u.then((f) => f()));
      window.removeEventListener("keydown", onKey);
    };
  });
</script>

<main class="flyout">
  <header>
    <select bind:value={presetIndex} onchange={reopenCurrent} aria-label="Upload limit">
      {#each presets as p, i (p.id)}
        <option value={i}>{p.label} · {bytes(p.bytes)}</option>
      {/each}
    </select>
    <button class="icon" onclick={api.hide} aria-label="Hide Kompacter">✕</button>
  </header>

  <Stage {mood} {progress} />

  <section class="info" aria-live="polite">
    {#if view.kind === "idle"}
      <h1>Drop a file</h1>
      <p>It shrinks to fit and lands on your clipboard.</p>
    {:else if view.kind === "reading"}
      <h1>{fileName(view.path)}</h1>
      <p>Reading…</p>
    {:else if view.kind === "big"}
      {@const { info, estimate } = view.found}
      <h1>{fileName(view.path)}</h1>
      <p>
        {bytes(info.size_bytes)} ·
        <span class="bad">✗ {bytes(info.size_bytes - target)} over your limit</span>
      </p>
      {#if estimate.video}
        <p>
          {#if estimate.video.fits}
            Compress makes it fit at {estimate.video.height}p · {estimate.video.quality} quality
          {:else}
            <span class="bad">✗ Too long to fit. Trim it first.</span>
          {/if}
        </p>
      {/if}
    {:else if view.kind === "busy"}
      <h1>{Math.round(progress * 100)}%</h1>
      <p>Compressing {fileName(view.path)}…</p>
    {:else if view.kind === "done"}
      {@const r = view.result}
      <h1>{r.fits ? "✓ Done" : "✗ Still too big"}</h1>
      <p>
        {bytes(r.original_bytes)} → <span class={r.fits ? "ok" : "bad"}>{bytes(r.output_bytes)}</span>
      </p>
      <p>{r.copied ? "Copied. Paste it anywhere." : `Saved to ${r.path}`}</p>
    {:else if view.kind === "fits"}
      <h1>Already fits</h1>
      <p>{bytes(view.size)} is under {bytes(target)}.</p>
    {:else if view.kind === "error"}
      <h1>{fileName(view.path)}</h1>
      <p class="bad">✗ {view.message}</p>
    {/if}
  </section>

  <footer>
    {#if view.kind === "big"}
      {@const path = view.path}
      <button class="primary" onclick={() => compress(path)}>Compress</button>
    {:else if view.kind === "busy"}
      <button onclick={api.cancel}>Cancel</button>
    {:else if view.kind !== "idle" && view.kind !== "reading"}
      <button onclick={() => (view = { kind: "idle" })}>Done</button>
    {/if}
  </footer>
</main>
