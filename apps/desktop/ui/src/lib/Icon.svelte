<!-- Thin line icons (SF Symbols-like), inline so there is no icon library. -->
<script module lang="ts">
  const ICONS = {
    film: '<rect x="3.5" y="5" width="17" height="14" rx="3"/><path d="M8 5v14M16 5v14M3.5 9.5H8M3.5 14.5H8M16 9.5h4.5M16 14.5h4.5"/>',
    music: '<path d="M9 17.5V6l10-2v11.5"/><circle cx="6.5" cy="17.5" r="2.5"/><circle cx="16.5" cy="15.5" r="2.5"/>',
    image: '<rect x="3.5" y="4.5" width="17" height="15" rx="3"/><circle cx="9" cy="10" r="1.8"/><path d="M20.5 15.5l-5-5-9 9"/>',
    folder: '<path d="M3.5 7.5a2 2 0 0 1 2-2h3.8l2 2h7.2a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>',
    file: '<path d="M6.5 3.5h7l4 4v11a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-13a2 2 0 0 1 2-2z"/><path d="M13.5 3.5v4h4"/>',
    gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2.8v2.4M12 18.8v2.4M2.8 12h2.4M18.8 12h2.4M5.5 5.5l1.7 1.7M16.8 16.8l1.7 1.7M5.5 18.5l1.7-1.7M16.8 7.2l1.7-1.7"/>',
    down: '<path d="M7 10l5 5 5-5"/>',
    back: '<path d="M14.5 6l-6 6 6 6"/>',
    check: '<path d="M5.5 12.5l4 4 9-9"/>',
    x: '<path d="M7 7l10 10M17 7L7 17"/>',
    okc: '<circle cx="12" cy="12" r="9"/><path d="M8.2 12.4l2.6 2.6 5-5.2"/>',
    xc: '<circle cx="12" cy="12" r="9"/><path d="M9.2 9.2l5.6 5.6M14.8 9.2l-5.6 5.6"/>',
    scissors: '<circle cx="6.5" cy="7" r="2.5"/><circle cx="6.5" cy="17" r="2.5"/><path d="M8.6 8.4L19.5 18M8.6 15.6L19.5 6"/>',
    squeeze: '<path d="M4 9.5h5.5V4M20 14.5h-5.5V20M9.5 9.5L4.5 4.5M14.5 14.5l5 5"/>',
    play: '<path d="M8.5 6.2v11.6L18 12z" fill="currentColor"/>',
    clip: '<rect x="5.5" y="4.5" width="13" height="16" rx="2.5"/><path d="M9.5 4.5V3.8a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1v.7"/>',
    clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
    lock: '<rect x="5.5" y="10.5" width="13" height="9.5" rx="2.5"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>',
    open: '<path d="M3.5 8V6.5a2 2 0 0 1 2-2h3.3l2 2h6.7a2 2 0 0 1 2 2V9"/><path d="M3.5 8.5h17l-1.6 9a2 2 0 0 1-2 1.5H7.1a2 2 0 0 1-2-1.5z"/>',
    spin: '<circle cx="12" cy="12" r="8" stroke-dasharray="30 60"/>',
    mail: '<rect x="3.5" y="5.5" width="17" height="13" rx="2.5"/><path d="M4.5 7l7.5 6 7.5-6"/>',
    custom: '<path d="M5 8h9M18 8h1M5 16h1M10 16h9"/><circle cx="16" cy="8" r="2"/><circle cx="8" cy="16" r="2"/>',
  } as const;
  export type IconName = keyof typeof ICONS;
</script>

<script lang="ts">
  let { name, small = false, class: cls = "" }: { name: IconName; small?: boolean; class?: string } = $props();
</script>

<svg class="ic {cls}" class:sm={small} viewBox="0 0 24 24" aria-hidden="true">{@html ICONS[name]}</svg>
