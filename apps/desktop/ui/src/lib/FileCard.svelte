<!-- File row: kind tile, name, one line of facts, optional remove button. -->
<script lang="ts">
  import type { MediaKind } from "./bindings/MediaKind";
  import Icon, { type IconName } from "./Icon.svelte";

  let { kind, name, meta, onremove }: { kind: MediaKind; name: string; meta: string; onremove?: () => void } = $props();

  const ICON: Record<MediaKind, IconName> = { video: "film", audio: "music", image: "image", folder: "folder", other: "file" };
</script>

<div class="file">
  <span class="tile" class:folder={kind === "folder"}><Icon name={ICON[kind]} /></span>
  <div class="meta">
    <div class="name" title={name}>{name}</div>
    <div class="sub">{meta}</div>
  </div>
  {#if onremove}
    <button class="icon-btn press" aria-label="Remove file" onclick={onremove}><Icon name="x" small /></button>
  {/if}
</div>

<style>
  .file { display: flex; align-items: center; gap: 12px; }
  .tile { width: 42px; height: 42px; border-radius: 11px; display: grid; place-items: center; background: var(--accent-soft); color: var(--accent-text); flex: none; }
  .tile.folder { background: var(--warn-soft); color: var(--warn); }
  .meta { min-width: 0; flex: 1; }
  .name { font-weight: 600; font-size: 15px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sub { margin: 0; }
</style>
