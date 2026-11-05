<!-- Result: Copied ✓ (or saved / still too big), with Show in Folder. -->
<script lang="ts">
  import * as api from "./api";
  import { bytes, fileName } from "./format";
  import Icon from "./Icon.svelte";

  let { result, label = "", onanother }: { result: api.Compressed; label?: string; onanother: () => void } = $props();

  let note = $state("");
  const ratio = $derived(result.original_bytes / Math.max(1, result.output_bytes));
  const title = $derived(
    !result.fits ? "Still too big" : result.copied ? "Copied — paste anywhere" : "Saved — ready to share",
  );
  const sub = $derived(
    result.untouched
      ? `${fileName(result.path)} is ${bytes(result.output_bytes)} — already fits${result.copied ? ", copied as-is" : ""}`
      : [fileName(result.path), bytes(result.output_bytes), label, ratio >= 1.1 ? `${ratio.toFixed(1)}× smaller` : ""].filter(Boolean).join(" · "),
  );

  async function reveal() {
    try {
      await api.showInFolder(result.path);
      note = "Opened in Explorer";
    } catch (e) {
      note = `Couldn't open the folder: ${e}`;
    }
  }
</script>

<section class="view center" aria-labelledby="doneTitle">
  <h2 class="h1 done-title" class:bad={!result.fits} id="doneTitle">
    <Icon name={result.fits ? "okc" : "xc"} />{title}
  </h2>
  <p class="sub">{sub}</p>
  {#if result.fits && result.copied}
    <p class="hint"><kbd>Ctrl</kbd><kbd>V</kbd> in any chat</p>
  {:else if result.fits}
    <p class="hint">Couldn't put it on the clipboard — drag it from the folder.</p>
  {:else}
    <p class="hint">Trim it shorter, or pick a bigger target.</p>
  {/if}
  <div class="row c btns">
    <button class="btn press" onclick={reveal}><Icon name="open" small />Show in Folder</button>
    <button class="btn plain press" onclick={onanother}>Kompact another</button>
  </div>
  <p class="note" aria-live="polite">{note}</p>
</section>

<style>
  .done-title { display: inline-flex; align-items: center; gap: 8px; animation: pop 280ms var(--spring) both; }
  .done-title :global(.ic) { width: 24px; height: 24px; color: var(--ok); stroke-width: 2; }
  .done-title.bad :global(.ic) { color: var(--bad); }
  @keyframes pop { from { opacity: 0; transform: scale(0.94); } to { opacity: 1; transform: none; } }
  .btns { margin-top: 14px; }
  .note { min-height: 18px; margin: 10px 0 0; font-size: 12px; color: var(--label-2); }
</style>
