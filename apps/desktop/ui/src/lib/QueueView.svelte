<!-- A dropped folder. The engine shrinks the media inside and zips it as one job, so one row. -->
<script lang="ts">
  import type { MediaInfo } from "./bindings/MediaInfo";
  import { bytes, fileName } from "./format";
  import Icon from "./Icon.svelte";

  let { info, target, oncompress, onremove }: { info: MediaInfo; target: number; oncompress: () => void; onremove: () => void } = $props();

  const over = $derived(info.size_bytes > target);
</script>

<section class="view" aria-labelledby="qName">
  <ul class="qlist" aria-label="Files to compress">
    <li>
      <span class="tile" aria-hidden="true"><Icon name="folder" small /></span>
      <span class="qname" id="qName" title={info.path}>{fileName(info.path)}</span>
      <span class="qsize">{bytes(info.size_bytes)}</span>
      <button class="icon-btn press" aria-label="Remove folder" onclick={onremove}><Icon name="x" small /></button>
    </li>
  </ul>
  <div class="status" class:ok={!over} class:bad={over}>
    <Icon name={over ? "xc" : "okc"} small />
    <span>{over ? `${bytes(info.size_bytes - target)} over — media will be shrunk, then zipped` : `Fits ${bytes(target)} — will be zipped as-is`}</span>
  </div>
  <button class="btn primary lg block press go" onclick={oncompress}>Compress &amp; copy as .zip</button>
</section>

<style>
  .qlist { list-style: none; margin: 12px 0 0; padding: 0; border-radius: var(--r-md); background: var(--group); }
  li { display: grid; grid-template-columns: 30px 1fr auto 32px; gap: 10px; align-items: center; padding: 6px 8px 6px 12px; font-size: 13px; }
  .tile { width: 30px; height: 30px; border-radius: 7px; display: grid; place-items: center; background: var(--warn-soft); color: var(--warn); }
  .qname { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-weight: 600; }
  .qsize { font-variant-numeric: tabular-nums; color: var(--label-2); font-size: 12px; }
  .go { margin-top: 14px; }
</style>
