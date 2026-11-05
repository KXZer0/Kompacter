<!-- Small glyph naming which app a preset targets (brand color), or a neutral icon. -->
<script lang="ts">
  import { BRAND_PATHS } from "./logos";
  import Icon from "./Icon.svelte";
  import type { Logo } from "./presets";

  let { logo }: { logo: Logo } = $props();
</script>

{#if logo === "mail" || logo === "custom"}
  <Icon name={logo} class="app b-{logo}" />
{:else}
  <svg class="app brand b-{logo}" viewBox="0 0 24 24" aria-hidden="true"><path d={BRAND_PATHS[logo]} /></svg>
{/if}

<style>
  :global(.app) { width: 15px; height: 15px; flex: none; vertical-align: -2px; }
  .brand { fill: currentColor; stroke: none; }
  :global(.b-mail), :global(.b-custom) { color: var(--label-2); }
</style>
