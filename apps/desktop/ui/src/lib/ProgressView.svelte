<!-- Under the press: big %, time left from the real progress rate, what we're making. -->
<script lang="ts">
  import { say } from "./live.svelte";

  let { progress, name, meta, oncancel }: { progress: number; name: string; meta: string; oncancel: () => void } = $props();

  const started = performance.now();
  const pct = $derived(Math.round(progress * 100));
  const left = $derived.by(() => {
    const elapsed = (performance.now() - started) / 1000;
    if (progress >= 0.995) return "Finishing…";
    if (progress < 0.03 || elapsed < 1) return "Estimating time…";
    const s = Math.ceil((elapsed * (1 - progress)) / progress);
    return s >= 90 ? `About ${Math.round(s / 60)} min left` : `About ${s} s left`;
  });

  let lastQuarter = -1;
  $effect(() => {
    const q = Math.floor(pct / 25);
    if (q !== lastQuarter && pct < 100) {
      lastQuarter = q;
      say(`Compressing ${name}, ${pct}%`);
    }
  });
</script>

<section class="view center" aria-label="Compressing {name}">
  <div role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-valuetext="{pct}%, {left}" aria-label="Compression progress">
    <div class="pct">{pct}%</div>
  </div>
  <p class="sub first">{left}</p>
  <p class="sub">{meta}</p>
  <button class="btn press cancel" onclick={oncancel}>Cancel</button>
</section>

<style>
  .pct { font-size: 34px; line-height: 40px; font-weight: 650; letter-spacing: -0.03em; font-variant-numeric: tabular-nums; }
  .first { margin-top: 0; }
  .cancel { margin-top: 16px; }
</style>
