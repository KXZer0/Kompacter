<!--
  The signature stage: dashed drop zone, the blob (face, pointer-following eyes, spring squish)
  and the vertical hydraulic press that squeezes it as REAL progress comes in.
  Only transform/opacity are animated; nothing reads layout per frame.
-->
<script module lang="ts">
  export type StageSize = "lg" | "md" | "press";
  export type Mood = "idle" | "hungry" | "worried" | "busy" | "happy";
</script>

<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { reduced, Spring } from "./motion";

  let {
    size,
    mood,
    dragging = false,
    progress = 0,
    showDash = false,
  }: { size: StageSize; mood: Mood; dragging?: boolean; progress?: number; showDash?: boolean } = $props();

  let stageEl: HTMLDivElement;
  let pouchEl: HTMLDivElement;
  let springEl: HTMLDivElement;
  let shadowEl: HTMLDivElement;
  let faceEl: HTMLDivElement;
  let sparksEl: HTMLDivElement;
  let headT: HTMLDivElement;
  let headB: HTMLDivElement;

  let spring: Spring | undefined;
  const sp = () => (spring ??= new Spring(springEl, shadowEl));

  /* ── Eyes follow the pointer (centre cached; recomputed only on resize) ── */
  let cx = 0, cy = 0, px = 0, py = 0, eyeRaf = 0;
  function measure() {
    const r = stageEl.getBoundingClientRect();
    cx = r.left + r.width / 2;
    cy = r.top + r.height / 2;
  }
  function applyLook() {
    eyeRaf = 0;
    if (reduced() || document.hidden) return;
    const dx = px - cx, dy = py - cy, d = Math.hypot(dx, dy) || 1, m = Math.min(3.5, d / 50);
    faceEl.style.transform = `translate(${((dx / d) * m).toFixed(2)}px,${((dy / d) * m * 0.7).toFixed(2)}px)`;
  }
  /** Point the eyes at a client position (pointer, or a file being dragged in). */
  export function look(x: number, y: number) {
    px = x;
    py = y;
    if (!eyeRaf) eyeRaf = requestAnimationFrame(applyLook);
  }

  /** A quick squish: on click, and per finished file in a batch. */
  export function chomp() {
    if (size !== "press") sp().push({ sx: 3.2, sy: -4.2 });
  }

  function burst() {
    if (reduced()) return;
    sparksEl.classList.remove("burst");
    void sparksEl.offsetWidth; // restart the CSS animation
    sparksEl.classList.add("burst");
  }

  /** Bounce + sparks; also re-expands the blob after the press shrank it to a dot. */
  export function celebrate() {
    pouchEl.style.opacity = "";
    requestAnimationFrame(() => requestAnimationFrame(() => pouchEl.style.removeProperty("--ps")));
    if (reduced()) return;
    setTimeout(() => {
      sp().push({ y: -420, sy: 3.5, sx: -2.5 });
      burst();
    }, 60);
  }

  $effect(() => {
    const swell = dragging;
    untrack(() => sp().to(swell ? { sx: 1.08, sy: 1.08 } : { sx: 1, sy: 1 }));
  });

  /* ── Vertical hydraulic press: one continuous squeeze driven by smoothed progress ── */
  const BASE = 0.85, HALF = 58;
  const press = { raf: 0, c: 0, target: 0, last: 0, timers: [] as number[] };
  const Y = (i: number, d: number) => `translateY(${(i ? d : -d).toFixed(2)}px)`;
  const heads = () => [headT, headB];
  let openDist = 140;

  function headsTo(d: number) {
    headT.style.transform = Y(0, d);
    headB.style.transform = Y(1, d);
  }
  function headsAnim(frames: { d: number; o?: number }[], duration: number, easing: string) {
    heads().forEach((h, i) => {
      h.getAnimations().forEach((a) => a.cancel());
      const kf = frames.map((f) => ({ transform: Y(i, f.d), offset: f.o }));
      h.style.transform = kf[kf.length - 1].transform;
      h.animate(kf, { duration, easing });
    });
  }
  /** c = 0…1 squeeze; returns the plate distance from the centre. */
  function squash(c: number) {
    const sy = 1 - 0.5 * c, sx = 1 + 0.2 * c;
    pouchEl.style.transform = `scale(${(BASE * sx).toFixed(4)},${(BASE * sy).toFixed(4)})`;
    pouchEl.dataset.sq = String(c > 0.66 ? 3 : c > 0.33 ? 2 : 1);
    return HALF * BASE * sy + 1.5;
  }
  function resetBlob() {
    pouchEl.getAnimations().forEach((a) => a.cancel());
    pouchEl.style.transition = "";
    pouchEl.style.transform = "";
    pouchEl.style.opacity = "";
    delete pouchEl.dataset.sq;
  }
  function startPress() {
    stopPress();
    resetBlob();
    pouchEl.style.transition = "none";
    press.c = 0;
    press.last = performance.now();
    openDist = stageEl.clientHeight / 2 + 10; // one layout read per press, not per frame
    if (!reduced()) headsAnim([{ d: openDist }, { d: squash(0) }], 620, "cubic-bezier(.2,.7,.2,1)");
    const tick = (now: number) => {
      const dt = Math.min(0.05, (now - press.last) / 1000);
      press.last = now;
      press.c += (press.target - press.c) * (1 - Math.exp(-dt / 0.3)); // eased: never jumps
      if (reduced()) {
        pouchEl.style.transform = `scale(${(BASE * (1 - 0.6 * press.c)).toFixed(3)})`;
        pouchEl.style.opacity = (1 - 0.5 * press.c).toFixed(2);
      } else headsTo(squash(press.c));
      press.raf = requestAnimationFrame(tick);
    };
    press.raf = requestAnimationFrame(tick);
  }
  function stopPress(keepBlob = false) {
    cancelAnimationFrame(press.raf);
    press.raf = 0;
    press.timers.forEach(clearTimeout);
    press.timers = [];
    heads().forEach((h) => h.getAnimations().forEach((a) => a.cancel()));
    if (!keepBlob) {
      resetBlob();
      pouchEl.style.removeProperty("--ps");
    }
  }

  /** Final firm squeeze → dot → pop → plates retract. Resolves when the done view can show. */
  export function finish(): Promise<void> {
    return new Promise((resolve) => {
      const done = () => {
        stopPress(true);
        resetBlob();
        pouchEl.style.setProperty("--ps", "0.05"); // celebrate() springs it back
        resolve();
      };
      if (size !== "press") return resolve();
      cancelAnimationFrame(press.raf);
      if (reduced()) {
        pouchEl.style.opacity = "0";
        press.timers.push(window.setTimeout(done, 180));
        return;
      }
      const from = pouchEl.style.transform, d0 = HALF * BASE * (1 - 0.5 * press.c) + 1.5;
      const ease = "cubic-bezier(.45,0,.7,.2)";
      pouchEl.animate(
        [{ transform: from }, { transform: `scale(${BASE * 1.26},${BASE * 0.4})`, offset: 0.6 }, { transform: "scale(.04)" }],
        { duration: 440, easing: ease, fill: "forwards" },
      );
      headsAnim([{ d: d0 }, { d: HALF * BASE * 0.4 + 1.5, o: 0.6 }, { d: 3 }], 440, ease);
      press.timers.push(
        window.setTimeout(() => {
          pouchEl.getAnimations().forEach((a) => a.cancel());
          pouchEl.style.transform = "scale(.04)";
          pouchEl.style.opacity = "0";
          burst();
          headsAnim([{ d: 3 }, { d: openDist }], 520, "cubic-bezier(.3,0,.2,1)");
        }, 450),
        window.setTimeout(done, 760),
      );
    });
  }

  $effect(() => {
    press.target = progress;
  });
  $effect(() => {
    const pressing = size === "press";
    untrack(() => (pressing ? startPress() : press.raf && stopPress()));
  });

  onMount(() => {
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(stageEl);
    const onMove = (e: PointerEvent) => look(e.clientX, e.clientY);
    window.addEventListener("pointermove", onMove, { passive: true });
    return () => {
      ro.disconnect();
      window.removeEventListener("pointermove", onMove);
      cancelAnimationFrame(eyeRaf);
      stopPress();
      spring?.stop();
    };
  });
</script>

<div class="stage" data-size={size} class:dragging class:dash-on={showDash || dragging} bind:this={stageEl} aria-hidden="true">
  <div class="well"><svg class="dash"><rect /></svg></div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="pouch" data-mood={mood} bind:this={pouchEl} onpointerdown={chomp}>
    <div class="breathe">
      <div class="spring" bind:this={springEl}>
        <div class="blob"></div>
        <div class="gloss"></div>
        <div class="face" bind:this={faceEl}><span class="eye l"></span><span class="eye r"></span><span class="mouth"></span></div>
      </div>
    </div>
    <div class="shadow" bind:this={shadowEl}></div>
  </div>
  <div class="sparks" bind:this={sparksEl}>
    {#each [0, 45, 90, 135, 180, 225, 270, 315] as a (a)}<i style:--a="{a}deg"></i>{/each}
  </div>
  <div class="rig">
    <div class="head t" bind:this={headT}><i class="pole"></i><i class="collar"></i><i class="trap"></i><i class="plate"></i></div>
    <div class="head b" bind:this={headB}><i class="pole"></i><i class="collar"></i><i class="trap"></i><i class="plate"></i></div>
  </div>
</div>

<style>
  .stage { position: relative; flex: 1 0 auto; height: 188px; margin: 2px -4px 0; }
  .stage[data-size="md"] { height: 112px; }
  .stage[data-size="press"] { height: 252px; overflow: hidden; margin: 2px -20px 12px; }
  .stage[data-size="press"] .shadow { opacity: 0; }

  /* Drop zone: even SVG dashes; accent + marching while a file hovers */
  .well { position: absolute; inset: 6px 10px; border-radius: 28px; opacity: 0; transform: scale(0.94); transition: transform var(--t-spring) var(--spring), opacity var(--t-fast), background-color var(--t-fast); }
  .dash-on .well { opacity: 1; transform: none; }
  .dragging .well { background: var(--accent-soft); transform: scale(1.015); }
  .dash { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; }
  .dash rect { x: 1px; y: 1px; width: calc(100% - 2px); height: calc(100% - 2px); rx: 27px; ry: 27px; fill: none; stroke: var(--label-2); stroke-opacity: 0.38; stroke-width: 1.5px; stroke-dasharray: 6 7; stroke-linecap: round; transition: stroke-opacity var(--t-fast); }
  .dragging .dash rect { stroke: var(--accent-text); stroke-opacity: 1; stroke-width: 2.2px; animation: march 1.1s linear infinite; }
  @keyframes march { to { stroke-dashoffset: -26; } }

  /* The blob */
  .pouch { position: absolute; left: 50%; top: 50%; width: 132px; height: 116px; margin: -58px 0 0 -66px; transform: scale(var(--ps, 1)); transition: transform var(--t-spring) var(--spring), opacity 120ms linear; cursor: pointer; }
  .stage[data-size="md"] .pouch { --ps: 0.62; }
  .breathe { position: absolute; inset: 0; transform-origin: 50% 100%; animation: breathe 3.6s ease-in-out infinite; }
  @keyframes breathe { 0%, 100% { transform: none; } 50% { transform: scale(1.025, 0.985); } }
  [data-mood="worried"] .breathe { animation: wobble 2.6s ease-in-out infinite; }
  @keyframes wobble { 0%, 55%, 100% { transform: none; } 10% { transform: rotate(-4deg) scale(1.03, 0.97); } 22% { transform: rotate(3deg) scale(0.98, 1.02); } 34% { transform: rotate(-1.5deg); } 45% { transform: rotate(0.6deg); } }
  [data-mood="busy"] .breathe { animation: none; }
  .spring { position: absolute; inset: 0; transform-origin: 50% 95%; will-change: transform; }
  .blob { position: absolute; inset: 0; border-radius: 47% 53% 45% 55% / 55% 52% 48% 45%; background: linear-gradient(150deg, var(--pouch-a) 0%, var(--pouch-b) 55%, var(--pouch-c) 100%); box-shadow: inset 0 -12px 22px rgba(25, 45, 120, 0.22), inset 0 8px 14px rgba(255, 255, 255, 0.6), 0 10px 22px -8px rgba(40, 70, 160, 0.45); animation: spin 18s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .gloss { position: absolute; left: 20%; top: 11%; width: 36%; height: 20%; border-radius: 50%; background: radial-gradient(closest-side, rgba(255, 255, 255, 0.9), rgba(255, 255, 255, 0)); transform: rotate(-16deg); }
  .shadow { position: absolute; left: 18%; right: 18%; bottom: -6px; height: 12px; border-radius: 50%; background: radial-gradient(closest-side, rgba(0, 0, 0, 0.22), transparent); }

  /* Face + moods */
  .face { position: absolute; left: 50%; top: 44%; width: 52px; height: 28px; margin-left: -26px; transition: transform 180ms var(--ease); }
  .eye { position: absolute; top: 0; width: 8px; height: 13px; border-radius: 5px; background: var(--face); transform-origin: 50% 60%; animation: blink 5.4s infinite; }
  .eye.l { left: 9px; }
  .eye.r { right: 9px; }
  @keyframes blink { 0%, 95%, 100% { transform: none; } 97% { transform: scaleY(0.12); } }
  .mouth { position: absolute; left: 50%; top: 18px; width: 12px; height: 6px; margin-left: -6px; border: 2.2px solid var(--face); border-top: 0; border-radius: 0 0 12px 12px; }
  [data-mood="hungry"] .eye { height: 15px; width: 9px; }
  [data-mood="hungry"] .mouth { width: 13px; height: 13px; margin-left: -6.5px; border: 0; border-radius: 50%; background: var(--face); top: 17px; }
  [data-mood="worried"] .eye { height: 10px; top: 3px; }
  [data-mood="worried"] .mouth { top: 21px; height: 5px; border: 2.2px solid var(--face); border-bottom: 0; border-radius: 12px 12px 0 0; }
  [data-mood="busy"] .eye { height: 3px; width: 10px; top: 6px; border-radius: 2px; animation: none; }
  [data-mood="busy"] .eye.l { transform: rotate(14deg); }
  [data-mood="busy"] .eye.r { transform: rotate(-14deg); }
  [data-mood="busy"] .mouth { width: 7px; height: 7px; margin-left: -3.5px; border: 2px solid var(--face); border-radius: 50%; }
  [data-mood="busy"]:global([data-sq="2"]) .eye.l { transform: rotate(22deg); }
  [data-mood="busy"]:global([data-sq="2"]) .eye.r { transform: rotate(-22deg); }
  [data-mood="busy"]:global([data-sq="2"]) .mouth { width: 6px; height: 6px; margin-left: -3px; }
  [data-mood="busy"]:global([data-sq="3"]) .eye { height: 2.5px; }
  [data-mood="busy"]:global([data-sq="3"]) .eye.l { transform: rotate(30deg) scaleX(1.15); }
  [data-mood="busy"]:global([data-sq="3"]) .eye.r { transform: rotate(-30deg) scaleX(1.15); }
  [data-mood="busy"]:global([data-sq="3"]) .mouth { width: 4px; height: 4px; margin-left: -2px; top: 19px; }
  [data-mood="happy"] .eye { background: none; border: 2.4px solid var(--face); border-bottom: 0; border-radius: 10px 10px 0 0; height: 7px; width: 11px; top: 4px; animation: none; }
  [data-mood="happy"] .mouth { width: 18px; height: 9px; margin-left: -9px; border: 0; background: var(--face); border-radius: 0 0 18px 18px; }

  /* Sparks */
  .sparks i { position: absolute; left: 50%; top: 50%; width: 6px; height: 6px; margin: -3px; border-radius: 50%; background: var(--accent); opacity: 0; }
  .sparks i:nth-child(even) { background: var(--pouch-b); }
  .sparks:global(.burst) i { animation: spark 420ms var(--ease) forwards; }
  @keyframes spark { 0% { opacity: 1; transform: rotate(var(--a)) translateY(-56px) scale(1); } 100% { opacity: 0; transform: rotate(var(--a)) translateY(-96px) scale(0.2); } }

  /* Vertical hydraulic press: pole → hazard collar → trapezoid → plate (W = 124px) */
  .rig { position: absolute; inset: 0; pointer-events: none; opacity: 0; transition: opacity var(--t-fast); --metal: linear-gradient(90deg, var(--m5) 0%, var(--m2) 22%, var(--m3) 38%, var(--m4) 60%, var(--m2) 80%, var(--m1) 100%); }
  .stage[data-size="press"] .rig { opacity: 1; }
  .head { position: absolute; left: 50%; width: 124px; margin-left: -62px; display: flex; flex-direction: column; align-items: center; will-change: transform; transform: translateY(-150px); }
  .head.t { bottom: 50%; }
  .head.b { top: 50%; flex-direction: column-reverse; transform: translateY(150px); }
  .head i { display: block; flex: none; }
  .pole { width: 27%; height: 200px; background: var(--metal); box-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.3); }
  .collar { width: 35%; height: 14px; border-radius: 3px; background: repeating-linear-gradient(45deg, #f5c400 0 5px, #141414 5px 10px); box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.4), inset 0 -1px 0 rgba(0, 0, 0, 0.45), 0 0 0 0.5px rgba(0, 0, 0, 0.6); }
  .trap { width: 82%; height: 26px; background: linear-gradient(180deg, rgba(255, 255, 255, 0.28), rgba(0, 0, 0, 0.16)), var(--metal); }
  .t .trap { clip-path: polygon(11.6% 0, 88.4% 0, 100% 100%, 0 100%); }
  .b .trap { clip-path: polygon(0 0, 100% 0, 88.4% 100%, 11.6% 100%); }
  .plate { width: 100%; height: 12px; border-radius: 2px; background: linear-gradient(180deg, rgba(255, 255, 255, 0.55), rgba(255, 255, 255, 0) 45%, rgba(0, 0, 0, 0.2)), var(--metal); box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), inset 0 -1px 0 rgba(0, 0, 0, 0.28), 0 0 0 0.5px rgba(0, 0, 0, 0.4); }
  .t .plate { box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), inset 0 -1px 0 rgba(0, 0, 0, 0.28), 0 0 0 0.5px rgba(0, 0, 0, 0.4), 0 8px 10px -6px rgba(0, 0, 0, 0.45); }
  @media (prefers-reduced-motion: reduce) { .rig { display: none; } }
</style>
