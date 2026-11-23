<script lang="ts">
  import type { AppDetails } from '$lib/api';

  // Czysto prezentacyjna ikona. Szczegóły (nazwa/ikona) dostarcza strumień
  // `load_apps` z backendu - komponent tylko je wyświetla.
  let {
    pkg,
    details = null,
    loading = false,
    size = 40
  }: {
    pkg: string;
    details?: AppDetails | null;
    loading?: boolean;
    size?: number;
  } = $props();

  const letter = $derived((details?.label ?? pkg.split('.').pop() ?? '?').charAt(0).toUpperCase());
</script>

<div class="relative shrink-0 overflow-hidden rounded-xl" style="width:{size}px;height:{size}px">
  {#if details?.icon}
    {#if details.adaptive}
      <!-- ikona adaptacyjna: tło + pierwszy plan (108dp, strefa bezpieczna 72dp => skala 1.5) -->
      <div
        class="absolute inset-0"
        style="background:{details.iconBg ?? '#fff'};{details.iconBgImage ? `background-image:url(${details.iconBgImage});background-size:150%;background-position:center;` : ''}"
      ></div>
      <img src={details.icon} alt="" class="absolute inset-0 h-full w-full scale-150 object-contain" draggable="false" />
    {:else}
      <img src={details.icon} alt="" class="h-full w-full animate-fade-in object-contain" draggable="false" />
    {/if}
  {:else}
    <div
      class="flex h-full w-full items-center justify-center bg-ink-600 font-semibold {loading ? 'animate-pulse text-neutral-600' : 'text-neutral-500'}"
      style="font-size:{size * 0.42}px"
    >
      {letter}
    </div>
  {/if}
</div>
