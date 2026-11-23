<script lang="ts">
  import { t } from '$lib/i18n';
  import SamsungPanel from '$lib/vendors/SamsungPanel.svelte';
  import XiaomiPanel from '$lib/vendors/XiaomiPanel.svelte';
  import MotorolaPanel from '$lib/vendors/MotorolaPanel.svelte';
  import SamsungFlasherPanel from '$lib/vendors/SamsungFlasherPanel.svelte';

  let { active = true }: { active?: boolean } = $props();

  type Sub = 'samsung' | 'flasher' | 'xiaomi' | 'motorola';
  let sub = $state<Sub>('samsung');
  const tabs: { id: Sub; label: string }[] = [
    { id: 'samsung', label: 'Samsung' },
    { id: 'flasher', label: 'Samsung Flasher' },
    { id: 'xiaomi', label: 'Xiaomi' },
    { id: 'motorola', label: 'Motorola' }
  ];
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex flex-wrap items-center justify-between gap-3">
    <h2 class="view-title">{$t('vendors.title')}</h2>
    <div class="flex rounded-lg border border-ink-500 bg-ink-800 p-0.5">
      {#each tabs as tb (tb.id)}
        <button
          class="rounded-md px-4 py-1.5 text-sm transition-colors {sub === tb.id ? 'bg-gold/15 text-gold' : 'text-neutral-400 hover:text-neutral-200'}"
          onclick={() => (sub = tb.id)}
        >{tb.label}</button>
      {/each}
    </div>
  </div>

  <!-- panele zostają zamontowane (zadania flashowania przeżyją przełączanie) -->
  <div class:hidden={sub !== 'samsung'}><SamsungPanel active={active && sub === 'samsung'} /></div>
  <div class:hidden={sub !== 'flasher'}><SamsungFlasherPanel active={active && sub === 'flasher'} /></div>
  <div class:hidden={sub !== 'xiaomi'}><XiaomiPanel active={active && sub === 'xiaomi'} /></div>
  <div class:hidden={sub !== 'motorola'}><MotorolaPanel /></div>
</section>
