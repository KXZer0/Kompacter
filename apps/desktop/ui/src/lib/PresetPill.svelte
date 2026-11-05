<!-- "Discord · 20 MB" pill in the header, with a menu to switch target. -->
<script lang="ts">
  import type { Preset } from "./bindings/Preset";
  import AppLogo from "./AppLogo.svelte";
  import Icon from "./Icon.svelte";
  import { bytes } from "./format";
  import { logoFor, note, shortName } from "./presets";

  let {
    presets,
    selected,
    onselect,
    onedit,
  }: { presets: Preset[]; selected: Preset; onselect: (id: string) => void; onedit: () => void } = $props();

  let open = $state(false);
  let btn: HTMLButtonElement;
  let menu = $state<HTMLDivElement>();

  const label = $derived(`${shortName(selected)} · ${bytes(selected.bytes)}`);

  function toggle() {
    open = !open;
    if (open) queueMicrotask(() => (menu?.querySelector<HTMLElement>('[aria-checked="true"]') ?? menu?.querySelector<HTMLElement>("button"))?.focus());
  }
  function close(refocus: boolean) {
    open = false;
    if (refocus) btn.focus();
  }
  function pick(id: string) {
    onselect(id);
    close(true);
  }
  function keys(e: KeyboardEvent) {
    const items = [...menu!.querySelectorAll<HTMLElement>("button")];
    const i = items.indexOf(document.activeElement as HTMLElement);
    const go = (n: number) => (e.preventDefault(), items[(n + items.length) % items.length].focus());
    if (e.key === "ArrowDown") go(i + 1);
    else if (e.key === "ArrowUp") go(i - 1);
    else if (e.key === "Home") go(0);
    else if (e.key === "End") go(items.length - 1);
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); }
    else if (e.key === "Tab") close(false);
  }
  function outside(e: PointerEvent) {
    if (open && !(e.target as Element).closest(".preset-wrap")) close(false);
  }
</script>

<svelte:window onpointerdown={outside} />

<div class="preset-wrap">
  <button
    bind:this={btn}
    class="preset press"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label="Target size: {label}. Change"
    onclick={toggle}
  >
    <AppLogo logo={logoFor(selected)} /><span>{label}</span><Icon name="down" small />
  </button>

  {#if open}
    <div class="menu" role="menu" aria-label="Target size" tabindex="-1" bind:this={menu} onkeydown={keys}>
      {#each presets as p (p.id)}
        <button role="menuitemradio" aria-checked={p.id === selected.id} onclick={() => pick(p.id)}>
          <span class="chk"><Icon name="check" small /></span>
          <AppLogo logo={logoFor(p)} />{shortName(p)}{note(p) ? ` · ${note(p)}` : ""}
          <small>{bytes(p.bytes)}</small>
        </button>
      {/each}
      <hr />
      <button role="menuitem" onclick={() => (close(false), onedit())}>Edit targets…</button>
    </div>
  {/if}
</div>

<style>
  .preset-wrap { position: relative; }
  .preset { display: inline-flex; align-items: center; gap: 6px; height: 30px; padding: 0 10px 0 12px; border: 0; border-radius: 999px; background: var(--fill); font-weight: 600; font-size: 13px; }
  .preset:hover { background: var(--fill-hover); }
  .menu {
    position: absolute; top: 36px; left: -4px; z-index: 40; min-width: 240px; padding: 6px; border-radius: 14px;
    background: var(--mat-strong); backdrop-filter: blur(30px) saturate(1.8); box-shadow: var(--shadow);
    transform-origin: top left; animation: pop 200ms var(--spring);
  }
  @keyframes pop { from { opacity: 0; transform: scale(0.94); } to { opacity: 1; transform: none; } }
  .menu button { display: flex; align-items: center; gap: 10px; width: 100%; min-height: 32px; padding: 0 10px; border: 0; border-radius: 8px; background: transparent; text-align: left; font-size: 13px; }
  .menu button:hover, .menu button:focus-visible { background: var(--accent); color: #fff; outline: none; }
  .menu button:hover :global(.app), .menu button:focus-visible :global(.app) { color: #fff; }
  .menu small { margin-left: auto; color: var(--label-2); font-size: 12px; }
  .menu button:hover small, .menu button:focus-visible small { color: rgba(255, 255, 255, 0.85); }
  .chk { width: 14px; display: grid; place-items: center; opacity: 0; }
  [aria-checked="true"] .chk { opacity: 1; }
  hr { border: 0; border-top: 1px solid var(--sep); margin: 5px 8px; }
</style>
