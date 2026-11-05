<!-- The drop zone: dashed outline, the blob, and the hydraulic press while compressing. -->
<script module lang="ts">
  export type Mood = "idle" | "over" | "big" | "busy" | "done";
</script>

<script lang="ts">
  let { mood, progress = 0 }: { mood: Mood; progress?: number } = $props();

  // The press squashes the blob: flatter and a bit wider as progress goes up.
  const sx = $derived(mood === "busy" ? 1 + 0.15 * progress : 1);
  const sy = $derived(mood === "busy" ? 1 - 0.6 * progress : 1);
</script>

<section class="stage" data-mood={mood} style:--p={progress} aria-hidden="true">
  <svg class="dash"><rect x="1" y="1" rx="20" ry="20" /></svg>
  <div class="press top"><i class="pole"></i><i class="collar"></i><i class="body"></i><i class="plate"></i></div>
  <div class="blob" style:--sx={sx} style:--sy={sy}>
    <span class="eyes"></span><span class="mouth"></span>
  </div>
  <div class="press bottom"><i class="plate"></i><i class="body"></i><i class="collar"></i><i class="pole"></i></div>
</section>

<style>
  .stage { position: relative; flex: 1; display: grid; place-items: center; overflow: hidden; }
  .dash { position: absolute; inset: 0; width: 100%; height: 100%; }
  .dash rect {
    width: calc(100% - 2px); height: calc(100% - 2px);
    fill: none; stroke: var(--muted); stroke-opacity: 0.35;
    stroke-width: 1.5; stroke-dasharray: 6 7; stroke-linecap: round;
    transition: stroke 0.2s, stroke-width 0.2s;
  }
  [data-mood="over"] .dash rect { stroke: var(--accent); stroke-opacity: 1; stroke-width: 2.5; animation: march 1s linear infinite; }
  [data-mood="busy"] .dash, [data-mood="done"] .dash { opacity: 0; }
  @keyframes march { to { stroke-dashoffset: -26; } }

  .blob {
    position: relative; width: 110px; height: 96px;
    border-radius: 50% 50% 46% 54% / 55% 52% 48% 45%;
    background: radial-gradient(circle at 35% 30%, #fff 0 6%, var(--blob-a) 22%, var(--blob-b) 100%);
    box-shadow: 0 14px 24px -12px rgba(30, 80, 200, 0.45);
    transform: scale(var(--sx), var(--sy));
    transition: transform 0.3s var(--spring);
    animation: breathe 3.2s ease-in-out infinite;
  }
  [data-mood="busy"] .blob, [data-mood="done"] .blob { animation: none; }
  [data-mood="done"] .blob { animation: pop 0.42s var(--spring); }
  @keyframes breathe { 50% { transform: scale(1.03, 0.97); } }
  @keyframes pop { from { transform: scale(0.2); } 60% { transform: scale(1.08); } }

  .eyes, .eyes::after {
    position: absolute; top: 38%; left: 36%;
    width: 7px; height: 10px; border-radius: 4px; background: #1d1d1f; content: "";
  }
  .eyes::after { top: 0; left: 22px; }
  .mouth {
    position: absolute; top: 58%; left: 50%; width: 14px; height: 7px; translate: -50% 0;
    border: 2.5px solid #1d1d1f; border-top: 0; border-radius: 0 0 10px 10px;
  }
  [data-mood="over"] .mouth { height: 10px; border-radius: 50%; border-top: 2.5px solid #1d1d1f; }
  [data-mood="big"] .mouth { border: 2.5px solid #1d1d1f; border-bottom: 0; border-radius: 10px 10px 0 0; }
  [data-mood="busy"] .eyes, [data-mood="busy"] .eyes::after { height: 3px; }

  .press {
    position: absolute; left: 50%; width: 150px; translate: -50% 0;
    display: flex; flex-direction: column; align-items: center;
    opacity: 0; transition: opacity 0.2s, transform 0.25s linear;
  }
  .press.top { top: 0; transform: translateY(calc(-60px + var(--p) * 52px)); }
  .press.bottom { bottom: 0; transform: translateY(calc(60px - var(--p) * 52px)); }
  [data-mood="busy"] .press { opacity: 1; }
  .press i { display: block; background: linear-gradient(90deg, var(--steel-b), var(--steel-a) 45%, var(--steel-b)); }
  .pole { width: 27%; height: 26px; }
  .press i.collar {
    width: 35%; height: 16px; border-radius: 4px;
    background: repeating-linear-gradient(45deg, #f5c400 0 6px, #1b1b1b 6px 12px);
  }
  .top .body { width: 82%; height: 30px; clip-path: polygon(12% 0, 88% 0, 100% 100%, 0 100%); }
  .bottom .body { width: 82%; height: 30px; clip-path: polygon(0 0, 100% 0, 88% 100%, 12% 100%); }
  .plate { width: 100%; height: 14px; border-radius: 3px; }

  @media (prefers-reduced-motion: reduce) {
    .blob, .dash rect { animation: none !important; }
    .press { display: none; }
  }
</style>
