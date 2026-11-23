<script lang="ts">
  import { LoaderCircle, CircleCheck, Square, TriangleAlert } from 'lucide-svelte';
  import { t } from '$lib/i18n';

  // Pasek postępu ładowania zakładki Aplikacje (lista pakietów -> nazwy i ikony z APK).
  let {
    stage,
    done,
    total,
    current,
    failed = 0,
    onCancel
  }: {
    stage: 'idle' | 'list' | 'details' | 'done' | 'cancelled' | 'error';
    done: number;
    total: number;
    current: string | null;
    failed?: number;
    onCancel: () => void;
  } = $props();

  const pct = $derived(total > 0 ? Math.min(100, (done / total) * 100) : 0);
  const busy = $derived(stage === 'list' || stage === 'details');
</script>

{#if stage !== 'idle'}
  <div class="card animate-fade-in space-y-2 !py-3 {stage === 'done' ? 'border-ok/30' : ''}">
    <div class="flex items-center gap-2 text-sm">
      {#if busy}
        <LoaderCircle size={16} class="animate-spin text-gold" />
      {:else if stage === 'error'}
        <TriangleAlert size={16} class="text-err" />
      {:else}
        <CircleCheck size={16} class="text-ok" />
      {/if}
      <span class="text-neutral-200">
        {#if stage === 'list'}{$t('apps.load.list')}
        {:else if stage === 'details'}{$t('apps.load.details')}
        {:else if stage === 'cancelled'}{$t('apps.load.cancelled')}
        {:else if stage === 'error'}{$t('apps.load.error')}
        {:else}{$t('apps.load.done')}{/if}
      </span>
      {#if stage === 'details' || stage === 'done' || stage === 'cancelled'}
        <span class="font-mono text-xs text-neutral-400">{done} / {total}</span>
        {#if failed > 0}<span class="text-xs text-warn">({$t('apps.load.noIcon')}: {failed})</span>{/if}
      {/if}
      <span class="ml-auto font-mono text-sm font-semibold text-gold">
        {#if stage === 'list'}…{:else}{pct.toFixed(0)}%{/if}
      </span>
      {#if busy}
        <button class="icon-btn" title={$t('apps.load.stop')} onclick={onCancel}><Square size={13} /></button>
      {/if}
    </div>

    <div class="relative h-2 overflow-hidden rounded-full bg-ink-600">
      {#if stage === 'list'}
        <!-- etap nieokreślony: lista pakietów jeszcze nie wróciła -->
        <div class="indeterminate absolute inset-y-0 w-1/3 rounded-full bg-gold"></div>
      {:else}
        <div
          class="h-full rounded-full bg-gradient-to-r from-gold-dim via-gold to-gold-soft transition-[width] duration-300 ease-out"
          style="width:{pct}%"
        ></div>
      {/if}
    </div>

    {#if stage === 'details' && current}
      <div class="truncate font-mono text-[11px] text-neutral-500">{current}</div>
    {/if}
  </div>
{/if}

<style>
  .indeterminate {
    animation: slide 1.2s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      left: -35%;
    }
    to {
      left: 100%;
    }
  }
</style>
