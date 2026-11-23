<script lang="ts">
  import { Square, CircleCheck, CircleX, CircleAlert } from 'lucide-svelte';
  import type { Job } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { humanBytes, humanDuration } from '$lib/format';

  let { job }: { job: Job } = $props();

  const p = $derived(job.progress);
  const pct = $derived(job.pct);
  const hasBytes = $derived((p?.bytesTotal ?? 0) > 0);
</script>

{#if job.running || job.result}
  <div class="card animate-fade-in space-y-2">
    <div class="flex items-center justify-between gap-3 text-sm">
      <div class="flex min-w-0 items-center gap-2">
        {#if job.running}
          <span class="h-2 w-2 shrink-0 animate-pulse rounded-full bg-gold"></span>
          <span class="text-neutral-200">{$t('job.running')}</span>
        {:else if job.result?.cancelled}
          <CircleAlert size={16} class="text-warn" /><span class="text-warn">{$t('job.cancelled')}</span>
        {:else if job.result && job.result.failures.length > 0}
          <CircleX size={16} class="text-err" /><span class="text-err">{$t('job.doneErrors')}</span>
        {:else}
          <CircleCheck size={16} class="text-ok" /><span class="text-ok">{$t('job.done')}</span>
        {/if}
      </div>
      <span class="font-mono text-lg font-semibold text-gold">{pct.toFixed(pct < 10 ? 1 : 0)}%</span>
    </div>

    <div class="relative h-2.5 overflow-hidden rounded-full bg-ink-600">
      <div
        class="h-full rounded-full bg-gradient-to-r transition-[width] duration-300 ease-out {job.result && job.result.failures.length > 0
          ? 'from-err/60 to-err'
          : 'from-gold-dim via-gold to-gold-soft'}"
        style="width: {pct}%"
      ></div>
      {#if job.running}
        <div class="shimmer pointer-events-none absolute inset-0"></div>
      {/if}
    </div>

    <div class="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-neutral-400 sm:grid-cols-4">
      <div>{$t('job.items')}: <span class="font-mono text-neutral-200">{p?.done ?? 0} / {p?.total ?? 0}</span></div>
      {#if hasBytes}
        <div>{$t('job.data')}: <span class="font-mono text-neutral-200">{humanBytes(p?.bytesDone)} / {humanBytes(p?.bytesTotal)}</span></div>
      {/if}
      {#if job.running && hasBytes}
        <div>{$t('job.speed')}: <span class="font-mono text-neutral-200">{humanBytes(job.speed)}/s</span></div>
        <div>{$t('job.eta')}: <span class="font-mono text-neutral-200">{humanDuration(job.eta)}</span></div>
      {/if}
      {#if job.result}
        <div>OK: <span class="font-mono text-ok">{job.result.ok}</span></div>
        <div>{$t('job.skipped')}: <span class="font-mono text-neutral-200">{job.result.skipped}</span></div>
        <div>{$t('job.errors')}: <span class="font-mono {job.result.failures.length ? 'text-err' : 'text-neutral-200'}">{job.result.failures.length}</span></div>
      {/if}
    </div>

    {#if job.running && p?.current}
      <div class="truncate font-mono text-xs text-neutral-300" title={p.current}>{p.current}</div>
    {/if}

    {#if job.result && job.result.failures.length > 0}
      <details class="text-xs">
        <summary class="cursor-pointer text-err">{$t('job.showErrors')} ({job.result.failures.length})</summary>
        <div class="mt-1 max-h-40 overflow-y-auto rounded border border-ink-600 bg-ink-900 p-2">
          {#each job.result.failures as f, i (i)}
            <div class="log-line text-err"><span class="text-neutral-400">{f.path}</span> — {f.error}</div>
          {/each}
        </div>
      </details>
    {/if}

    {#if job.running}
      <div class="flex justify-end">
        <button class="btn btn-danger !py-1.5" onclick={() => job.cancel()}><Square size={14} />{$t('common.cancel')}</button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .shimmer {
    background: linear-gradient(90deg, transparent 0%, rgba(255, 255, 255, 0.18) 50%, transparent 100%);
    background-size: 200% 100%;
    animation: shimmer 1.6s linear infinite;
  }
  @keyframes shimmer {
    from {
      background-position: 200% 0;
    }
    to {
      background-position: -200% 0;
    }
  }
</style>
