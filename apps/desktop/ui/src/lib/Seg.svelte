<!-- Segmented control = radiogroup with a sliding thumb. Arrow keys move and select. -->
<script lang="ts" generics="T extends string">
  let {
    options,
    value,
    onchange,
    label,
    disabled = false,
    describedby,
  }: {
    options: { v: T; label: string }[];
    value: T;
    onchange: (v: T) => void;
    label: string;
    disabled?: boolean;
    describedby?: string;
  } = $props();

  let el: HTMLDivElement;
  const index = $derived(Math.max(0, options.findIndex((o) => o.v === value)));

  function keys(e: KeyboardEvent) {
    const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const n = (index + d + options.length) % options.length;
    onchange(options[n].v);
    queueMicrotask(() => el.querySelectorAll<HTMLElement>('[role="radio"]')[n]?.focus());
  }
</script>

<div
  class="seg"
  role="radiogroup"
  aria-label={label}
  aria-describedby={describedby}
  aria-disabled={disabled}
  style:--n={options.length}
  style:--i={index}
  bind:this={el}
  onkeydown={keys}
  tabindex="-1"
>
  <span class="thumb-ind" aria-hidden="true"></span>
  {#each options as o (o.v)}
    <button role="radio" aria-checked={o.v === value} tabindex={o.v === value ? 0 : -1} onclick={() => onchange(o.v)}>{o.label}</button>
  {/each}
</div>
