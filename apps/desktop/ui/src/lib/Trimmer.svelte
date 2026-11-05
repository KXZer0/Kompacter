<!--
  Trim video/audio with live estimates. Handles work with pointer and keyboard; every change
  asks the engine (probe is cached) at most once per frame and ≥50 ms apart.
-->
<script module lang="ts">
  export type TrimRequest = { start: number; end: number; keepQuality: boolean; label: string };
</script>

<script lang="ts">
  import { onMount, untrack } from "svelte";
  import * as api from "./api";
  import type { Estimate } from "./bindings/Estimate";
  import type { MediaInfo } from "./bindings/MediaInfo";
  import { bytes, fileName } from "./format";
  import Icon from "./Icon.svelte";
  import { say } from "./live.svelte";
  import Seg from "./Seg.svelte";

  let {
    path,
    info,
    target,
    oncompress,
    onback,
  }: { path: string; info: MediaInfo; target: number; oncompress: (r: TrimRequest) => void; onback: () => void } = $props();

  const dur = $derived(Math.max(1, info.duration));
  let a = $state(0);
  let b = $state(untrack(() => info.duration)); // the parent re-keys this view per file
  let active = $state<"a" | "b">("a");
  let mode = $state<"keep" | "fit">("keep");
  let est = $state<Estimate | null>(null);
  let thumbs = $state<string[]>([]);
  let drag = $state<"a" | "b" | null>(null);
  let strip: HTMLDivElement;
  let thumbsEl: HTMLDivElement;
  let stripLeft = 0, stripWidth = 1; // measured on pointerdown, not per move

  /* ── Live estimate: coalesced to one in-flight request, rAF + ≥50 ms apart ── */
  let raf = 0, inFlight = false, dirty = false, lastSent = 0, timer = 0;
  function schedule() {
    if (raf || timer) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      const wait = 50 - (performance.now() - lastSent);
      if (wait > 0) timer = window.setTimeout(() => ((timer = 0), run()), wait);
      else run();
    });
  }
  async function run() {
    if (inFlight) return void (dirty = true);
    inFlight = true;
    lastSent = performance.now();
    try {
      est = (await api.inspect(path, target, { start: a, end: b })).estimate;
    } catch {
      /* keep the last estimate */
    } finally {
      inFlight = false;
      if (dirty) ((dirty = false), schedule());
    }
  }
  $effect(() => {
    void [a, b, target];
    schedule();
  });

  onMount(() => {
    api.thumbnails(path, 10).then((t) => (thumbs = t)).catch(() => {});
    return () => (cancelAnimationFrame(raf), clearTimeout(timer));
  });

  /* ── Handles ── */
  const sec = $derived(b - a);
  const fmtT = (s: number, dec = false) => {
    const m = Math.floor(s / 60), r = s - m * 60;
    return `${m}:${dec ? r.toFixed(1).padStart(4, "0") : String(Math.floor(r)).padStart(2, "0")}`;
  };
  function set(w: "a" | "b", t: number) {
    if (w === "a") a = Math.max(0, Math.min(t, b - 1));
    else b = Math.min(info.duration, Math.max(t, a + 1));
    active = w;
  }
  const tAt = (x: number) => Math.max(0, Math.min(1, (x - stripLeft) / stripWidth)) * info.duration;
  function grab(e: PointerEvent, w?: "a" | "b") {
    e.preventDefault();
    const r = thumbsEl.getBoundingClientRect();
    stripLeft = r.left;
    stripWidth = r.width || 1;
    if (!w) {
      const t = tAt(e.clientX);
      w = Math.abs(t - a) < Math.abs(t - b) ? "a" : "b";
      set(w, t);
    }
    drag = w;
    const h = strip.querySelector<HTMLElement>(`.handle.${w}`)!;
    h.setPointerCapture(e.pointerId);
    h.focus({ preventScroll: true });
  }
  function move(e: PointerEvent) {
    if (drag) set(drag, tAt(e.clientX));
  }
  function release() {
    if (!drag) return;
    drag = null;
    announce();
  }
  function keys(e: KeyboardEvent, w: "a" | "b") {
    const cur = w === "a" ? a : b, step = e.shiftKey ? 5 : 0.5;
    const t =
      e.key === "ArrowLeft" || e.key === "ArrowDown" ? cur - step
      : e.key === "ArrowRight" || e.key === "ArrowUp" ? cur + step
      : e.key === "PageDown" ? cur - 5
      : e.key === "PageUp" ? cur + 5
      : e.key === "Home" ? 0
      : e.key === "End" ? info.duration
      : null;
    if (t === null) return;
    e.preventDefault();
    set(w, t);
    announce();
  }

  /* ── Readout ── */
  const GRADE = { great: "Great", okay: "Okay", low: "Low" } as const;
  const keepBytes = $derived(est?.keep_quality_bytes ?? (info.size_bytes * sec) / dur);
  const keepOk = $derived(keepBytes <= target);
  const needCut = $derived(Math.ceil((keepBytes - target) / (keepBytes / Math.max(sec, 0.1))));
  const plan = $derived(est?.video ?? null);
  const fitOk = $derived(
    !est ? true : est.fits_as_is || (info.kind === "video" ? !!plan?.fits : est.audio_kbps !== null),
  );

  let annT = 0;
  function announce() {
    clearTimeout(annT);
    annT = window.setTimeout(() => {
      const what = mode === "keep"
        ? `About ${bytes(keepBytes)}. ${keepOk ? "Fits" : "Over the limit"}.`
        : plan ? `${plan.height}p, ${GRADE[plan.quality]} quality.` : fitOk ? "Fits." : "Too long to fit.";
      say(`${Math.round(sec)} seconds selected. ${what}`);
    }, 350);
  }

  function go() {
    if (mode === "keep" && !keepOk) {
      mode = "fit";
      announce();
      return;
    }
    const label = mode === "keep" || est?.fits_as_is ? "Original quality" : plan ? `${plan.height}p` : est?.audio_kbps ? `${est.audio_kbps} kbps` : "";
    oncompress({ start: a, end: b, keepQuality: mode === "keep", label });
  }

  const frameBg = (p: number) => {
    const h = 205 + 130 * p, sx = 18 + 64 * p, sy = 40 - 16 * Math.sin(p * Math.PI);
    return `radial-gradient(circle at ${sx}% ${sy}%, hsl(${h + 50} 100% 90%) 0 5%, transparent 13%), radial-gradient(130% 55% at 25% 104%, hsl(${h - 40} 38% 30%) 0 50%, transparent 51%), linear-gradient(180deg, hsl(${h} 62% 58%), hsl(${h + 35} 70% 78%) 70%)`;
  };
  const activeT = $derived(active === "a" ? a : b);
  const frame = $derived(thumbs.length ? thumbs[Math.round((activeT / dur) * (thumbs.length - 1))] : null);
</script>

<section class="view" aria-labelledby="trimName">
  <div class="tbar">
    <button class="icon-btn press" aria-label="Back" onclick={onback}><Icon name="back" /></button>
    <div class="meta">
      <div class="h2 name" id="trimName">{fileName(path)}</div>
      <div class="sub">{fmtT(a)} – {fmtT(b)} · {Math.round(sec)} s selected</div>
    </div>
  </div>

  <div class="preview" role="img" aria-label="Frame at {fmtT(activeT)}" style:background={frameBg(activeT / dur)}>
    {#if frame}<img src={frame} alt="" />{/if}
    <span class="pv-time">{fmtT(activeT, true)}</span>
  </div>

  <div class="strip" bind:this={strip} onpointerdown={(e) => grab(e)} onpointermove={move} onpointerup={release} onpointercancel={release} role="presentation">
    <div class="thumbs" bind:this={thumbsEl} aria-hidden="true">
      {#each { length: 10 }, i (i)}
        <i style:background={frameBg(i / 9)}>{#if thumbs[i]}<img src={thumbs[i]} alt="" />{/if}</i>
      {/each}
    </div>
    <div class="dim l" style:width="{(a / dur) * 100}%"></div>
    <div class="dim r" style:width="{100 - (b / dur) * 100}%"></div>
    <div class="sel" style:left="{(a / dur) * 100}%" style:right="{100 - (b / dur) * 100}%"></div>
    {#each [["a", a, "Trim start", 0, b - 1], ["b", b, "Trim end", a + 1, info.duration]] as const as [w, t, lbl, min, max] (w)}
      <div
        class="handle {w}"
        class:drag={drag === w}
        style:left="{(t / dur) * 100}%"
        role="slider"
        tabindex="0"
        aria-label={lbl}
        aria-valuemin={Math.round(min * 10) / 10}
        aria-valuemax={Math.round(max * 10) / 10}
        aria-valuenow={Math.round(t * 10) / 10}
        aria-valuetext="{w === 'a' ? 'Start' : 'End'} {fmtT(t)}"
        onpointerdown={(e) => (e.stopPropagation(), grab(e, w))}
        onkeydown={(e) => keys(e, w)}
        onfocus={() => (active = w)}
      ><span class="tip">{fmtT(t, true)}</span></div>
    {/each}
  </div>
  <div class="ticks" aria-hidden="true"><span>0:00</span><span>{fmtT(info.duration / 2)}</span><span>{fmtT(info.duration)}</span></div>

  <Seg
    label="Trim mode"
    value={mode}
    onchange={(v) => ((mode = v), announce())}
    options={[{ v: "keep", label: "Keep quality" }, { v: "fit", label: `Fit to ${bytes(target)}` }]}
  />

  <div class="readout">
    {#if mode === "keep"}
      <div><div class="ro-lbl">Estimated size · original quality</div><div class="ro-big">≈ {bytes(keepBytes)}</div></div>
      <span></span>
      <div class="ro-status" class:ok={keepOk} class:bad={!keepOk}>
        <Icon name={keepOk ? "okc" : "xc"} small />
        <span>{keepOk ? `Fits under ${bytes(target)}` : `${bytes(keepBytes - target)} over — cut ${needCut} s more, or Fit`}</span>
      </div>
    {:else if est?.fits_as_is}
      <div><div class="ro-lbl">Output</div><div class="ro-big">Original</div></div>
      <span class="badge g-great"><span class="dots" aria-hidden="true"><i></i><i></i><i></i></span>Great quality</span>
      <div class="ro-status ok"><Icon name="okc" small /><span>≈ {bytes(keepBytes)} · fits {bytes(target)} · no re-encode</span></div>
    {:else if plan}
      <div><div class="ro-lbl">Output</div><div class="ro-big">{plan.height}p</div></div>
      <span class="badge g-{plan.quality}"><span class="dots" aria-hidden="true"><i></i><i></i><i></i></span>{GRADE[plan.quality]} quality</span>
      <div class="ro-status" class:ok={plan.fits} class:bad={!plan.fits}>
        <Icon name={plan.fits ? "okc" : "xc"} small />
        <span>{plan.fits ? `Fits ${bytes(target)} · ${(plan.video_kbps / 1000).toFixed(1)} Mbps` : "Too long to fit — trim more"}</span>
      </div>
      {#if plan.quality === "low" && plan.fits}<div class="ro-lbl wide">Shorter selection = sharper video.</div>{/if}
    {:else}
      <div><div class="ro-lbl">Output</div><div class="ro-big">{est?.audio_kbps ? `${est.audio_kbps} kbps` : "—"}</div></div>
      <span></span>
      <div class="ro-status" class:ok={fitOk} class:bad={!fitOk}>
        <Icon name={fitOk ? "okc" : "xc"} small /><span>{fitOk ? `Fits ${bytes(target)}` : "Too long to fit — trim more"}</span>
      </div>
    {/if}
  </div>

  <div class="trim-actions">
    <button class="btn primary lg press" onclick={go} disabled={mode === "fit" && !fitOk}>
      {mode === "keep" ? (keepOk ? "Export & copy" : `Fit to ${bytes(target)} instead`) : "Compress & copy"}
    </button>
  </div>
</section>

<style>
  .tbar { display: flex; align-items: center; gap: 8px; margin: 0 0 12px; }
  .meta { flex: 1; min-width: 0; }
  .name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .meta .sub { margin: 0; }
  .preview { position: relative; aspect-ratio: 16 / 9; border-radius: 14px; overflow: hidden; }
  .preview img, .thumbs img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; animation: fade 200ms ease both; }
  @keyframes fade { from { opacity: 0; } }
  .pv-time { position: absolute; right: 10px; bottom: 10px; padding: 3px 9px; border-radius: 999px; background: rgba(0, 0, 0, 0.5); color: #fff; font-size: 12px; font-weight: 600; font-variant-numeric: tabular-nums; }
  .strip { position: relative; height: 48px; margin: 14px 14px 6px; touch-action: none; }
  .thumbs { position: absolute; inset: 0; display: flex; border-radius: 9px; overflow: hidden; }
  .thumbs i { position: relative; flex: 1; border-right: 1px solid rgba(0, 0, 0, 0.18); }
  .dim { position: absolute; top: 0; bottom: 0; background: rgba(10, 10, 14, 0.58); }
  .dim.l { left: 0; border-radius: 9px 0 0 9px; }
  .dim.r { right: 0; border-radius: 0 9px 9px 0; }
  .sel { position: absolute; top: -3px; bottom: -3px; border-top: 3px solid #ffcc00; border-bottom: 3px solid #ffcc00; pointer-events: none; }
  .handle { position: absolute; top: -3px; bottom: -3px; width: 14px; background: #ffcc00; display: grid; place-items: center; cursor: ew-resize; touch-action: none; transition: transform 120ms var(--spring); }
  .handle::before { content: ""; width: 2.5px; height: 16px; border-radius: 2px; background: rgba(0, 0, 0, 0.55); }
  .handle::after { content: ""; position: absolute; inset: -6px -12px; }
  .handle.a { border-radius: 7px 0 0 7px; transform: translateX(-100%); }
  .handle.b { border-radius: 0 7px 7px 0; }
  .handle.a.drag { transform: translateX(-100%) scaleY(1.12); }
  .handle.b.drag { transform: scaleY(1.12); }
  .handle:focus-visible { outline-offset: 3px; }
  .tip { position: absolute; bottom: calc(100% + 8px); left: 50%; transform: translateX(-50%); padding: 2px 8px; border-radius: 999px; background: var(--mat-dark); color: #fff; font-size: 11px; font-weight: 600; white-space: nowrap; opacity: 0; transition: opacity 120ms; pointer-events: none; font-variant-numeric: tabular-nums; }
  .drag .tip, .handle:focus-visible .tip { opacity: 1; }
  .ticks { display: flex; justify-content: space-between; margin: 0 14px; font-size: 11px; color: var(--label-2); font-variant-numeric: tabular-nums; }
  .readout { margin-top: 12px; padding: 14px 16px; border-radius: var(--r-lg); background: var(--group); display: grid; grid-template-columns: 1fr auto; align-items: end; gap: 2px 12px; }
  .ro-lbl { font-size: 12px; color: var(--label-2); }
  .ro-lbl.wide { grid-column: 1 / -1; margin-top: 4px; }
  .ro-big { font-size: 28px; line-height: 34px; font-weight: 650; letter-spacing: -0.02em; font-variant-numeric: tabular-nums; }
  .ro-status { grid-column: 1 / -1; display: flex; align-items: center; gap: 6px; font-size: 13px; font-weight: 600; margin-top: 6px; }
  .badge { display: inline-flex; align-items: center; gap: 6px; height: 24px; padding: 0 10px; border-radius: 999px; font-size: 12px; font-weight: 600; margin-bottom: 5px; }
  .g-great { background: var(--ok-soft); color: var(--ok); }
  .g-okay { background: var(--accent-soft); color: var(--accent-text); }
  .g-low { background: var(--bad-soft); color: var(--bad); }
  .dots { display: inline-flex; gap: 2px; }
  .dots i { width: 5px; height: 5px; border-radius: 50%; background: currentColor; opacity: 0.3; }
  .g-great .dots i, .g-okay .dots i:nth-child(-n + 2), .g-low .dots i:first-child { opacity: 1; }
  .trim-actions { display: flex; justify-content: flex-end; margin-top: 14px; }
</style>
