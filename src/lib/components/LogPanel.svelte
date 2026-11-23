<script lang="ts">
  import { Trash2, Copy, ArrowDownToLine } from 'lucide-svelte';
  import { log, clearLog } from '$lib/stores.svelte';
  import type { LogLevel } from '$lib/api';
  import { t } from '$lib/i18n';

  let filter = $state<'all' | LogLevel>('all');
  let follow = $state(true);
  let box: HTMLDivElement | undefined = $state();

  const color: Record<LogLevel, string> = {
    info: 'text-neutral-400',
    ok: 'text-ok',
    warn: 'text-warn',
    error: 'text-err'
  };

  // render ostatnich 600 wpisów - przy dużych transferach log ma tysiące linii
  const visible = $derived(
    (filter === 'all' ? log.entries : log.entries.filter((e) => e.level === filter)).slice(-600)
  );

  $effect(() => {
    void visible.length;
    if (follow && box) queueMicrotask(() => box && (box.scrollTop = box.scrollHeight));
  });

  function onScroll() {
    if (!box) return;
    follow = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  }

  async function copyAll() {
    const txt = log.entries.map((e) => `${e.ts} [${e.level}] ${e.message}`).join('\n');
    try {
      await navigator.clipboard.writeText(txt);
    } catch {
      /* brak schowka */
    }
  }
</script>

<aside class="flex w-[340px] shrink-0 flex-col border-l border-ink-500 bg-ink-800">
  <div class="flex items-center gap-1 border-b border-ink-500 px-3 py-2">
    <span class="mr-auto text-sm font-medium text-neutral-300">{$t('log.title')}</span>
    <select bind:value={filter} class="input !w-auto !py-1 text-xs">
      <option value="all">{$t('log.all')}</option>
      <option value="ok">OK</option>
      <option value="warn">{$t('log.warn')}</option>
      <option value="error">{$t('log.error')}</option>
    </select>
    <button class="icon-btn" title={$t('common.copy')} onclick={copyAll}><Copy size={14} /></button>
    <button class="icon-btn" title={$t('log.clear')} onclick={clearLog}><Trash2 size={14} /></button>
  </div>
  <div class="flex-1 overflow-y-auto p-2" bind:this={box} onscroll={onScroll}>
    {#if visible.length === 0}
      <div class="p-2 text-xs text-neutral-600">{$t('log.empty')}</div>
    {:else}
      {#each visible as l (l.id)}
        <div class="log-line {color[l.level]}"><span class="text-neutral-600">{l.ts}</span> {l.message}</div>
      {/each}
    {/if}
  </div>
  {#if !follow}
    <button class="m-2 btn justify-center !py-1 text-xs" onclick={() => { follow = true; if (box) box.scrollTop = box.scrollHeight; }}>
      <ArrowDownToLine size={13} />{$t('log.follow')}
    </button>
  {/if}
</aside>
