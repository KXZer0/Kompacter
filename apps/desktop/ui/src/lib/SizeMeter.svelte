<!-- File size against the limit: a capsule with a limit tick; the over-limit part is red. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { bytes } from "./format";

  let { size, target }: { size: number; target: number } = $props();

  const max = $derived(Math.max(size, target) * 1.08);
  const okPart = $derived(`${Math.min(100, (target / size) * 100)}%`);
  let shown = $state(0); // grows in from 0 once, with the spring

  onMount(() => {
    const id = requestAnimationFrame(() => requestAnimationFrame(() => (shown = 1)));
    return () => cancelAnimationFrame(id);
  });
</script>

<div class="meter" role="img" aria-label="{bytes(size)} file against a {bytes(target)} limit">
  <div class="track">
    <div class="fill" style:--okp={okPart} style:transform="scaleX({shown * (size / max)})"></div>
    <div class="limit" style:left="{(target / max) * 100}%"><span>{bytes(target)}</span></div>
  </div>
</div>

<style>
  .meter { margin: 30px 0 8px; }
  .track { position: relative; height: 6px; border-radius: 3px; background: var(--fill-2); }
  .fill { position: absolute; inset: 0; border-radius: 3px; transform-origin: 0 50%; transition: transform 420ms var(--spring); background: linear-gradient(90deg, var(--label-2) 0 var(--okp), var(--bad) var(--okp)); }
  .limit { position: absolute; top: -5px; bottom: -5px; width: 2px; margin-left: -1px; border-radius: 1px; background: var(--label); }
  .limit span { position: absolute; top: -20px; left: 50%; transform: translateX(-50%); font-size: 11px; font-weight: 600; white-space: nowrap; color: var(--label-2); }
</style>
