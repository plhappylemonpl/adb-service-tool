<script lang="ts">
  import { open as openUrl } from '@tauri-apps/plugin-shell';
  import {
    Sparkles, X, Download, LoaderCircle, CircleCheck, CircleX, ShieldCheck, ExternalLink, FolderOpen, TerminalSquare, Clock, EyeOff
  } from 'lucide-svelte';
  import { api } from '$lib/api';
  import { updater } from '$lib/updater.svelte';
  import { t, lang } from '$lib/i18n';
  import { humanBytes, errText } from '$lib/format';
  import { toast } from '$lib/components/Toast.svelte';

  const info = $derived(updater.info);
  const job = updater.job;
  const p = $derived(job.progress);
  const pct = $derived(job.pct);
  const notes = $derived(info ? ($lang === 'pl' ? info.notesPl || info.notesEn : info.notesEn || info.notesPl) : '');
  const busy = $derived(job.running);
  let pkgBusy = $state(false);

  function close() {
    if (busy) return;
    updater.open = false;
  }

  async function installPackage(path: string) {
    pkgBusy = true;
    try {
      await api.updateInstallPackage(path);
      toast('ok', $t('update.pkgDone'));
    } catch (e) {
      toast('error', $t('update.pkgFailed'), errText(e));
    } finally {
      pkgBusy = false;
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast('ok', $t('update.copied'));
    } catch {
      /* brak schowka */
    }
  }
</script>

{#if updater.open && info}
  <div class="fixed inset-0 z-[70] flex items-center justify-center bg-black/75 p-6 backdrop-blur-sm" role="presentation">
    <div class="card w-full max-w-xl animate-fade-in space-y-4 border-gold/40 shadow-[0_0_40px_rgba(201,162,73,0.15)]" role="dialog" aria-modal="true">
      <div class="flex items-start gap-3">
        <div class="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl border border-gold/50 bg-gold/10 text-gold">
          <Sparkles size={22} />
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-base font-semibold text-neutral-100">
            {info.available ? $t('update.title') : $t('update.upToDate')}
          </h3>
          <p class="text-xs text-neutral-500">
            {$t('update.current')}: <span class="font-mono text-neutral-300">{info.current}</span>
            {#if info.latest}
              → {$t('update.latest')}: <span class="font-mono text-gold">{info.latest}</span>
            {/if}
            {#if info.publishedAt}· {new Date(info.publishedAt).toLocaleDateString()}{/if}
          </p>
        </div>
        <button class="text-neutral-500 hover:text-neutral-200 disabled:opacity-30" onclick={close} disabled={busy} title={$t('common.close')}>
          <X size={18} />
        </button>
      </div>

      {#if notes}
        <div class="max-h-56 overflow-y-auto rounded-lg border border-ink-600 bg-ink-900 p-3">
          <div class="mb-1 text-[11px] uppercase tracking-wider text-neutral-500">{$t('update.notes')}</div>
          <pre class="select-text whitespace-pre-wrap font-sans text-sm leading-relaxed text-neutral-300">{notes}</pre>
        </div>
      {/if}

      {#if info.available}
        {#if info.asset}
          <div class="flex items-center gap-2 text-xs text-neutral-400">
            <ShieldCheck size={14} class="shrink-0 text-ok" />
            <span class="truncate font-mono" title={info.asset.filename}>{info.asset.filename}</span>
            <span class="ml-auto shrink-0">{humanBytes(info.asset.size)} · SHA-256</span>
          </div>
        {/if}

        {#if busy || p}
          <div class="space-y-1.5">
            <div class="relative h-2.5 overflow-hidden rounded-full bg-ink-600">
              <div class="h-full rounded-full bg-gradient-to-r from-gold-dim via-gold to-gold-soft transition-[width] duration-300" style="width: {pct}%"></div>
            </div>
            <div class="flex justify-between font-mono text-xs text-neutral-400">
              <span class="truncate">{p?.current ?? ''}</span>
              <span>{humanBytes(p?.bytesDone ?? 0)} / {humanBytes(p?.bytesTotal ?? info.asset?.size ?? 0)} · {pct.toFixed(0)}%</span>
            </div>
          </div>
        {/if}

        {#if updater.installError}
          <div class="flex items-start gap-2 rounded-lg border border-err/40 bg-err/10 p-2.5 text-xs text-err">
            <CircleX size={14} class="mt-px shrink-0" /><span class="select-text whitespace-pre-wrap">{updater.installError}</span>
          </div>
        {/if}

        {#if updater.outcome?.action === 'restart'}
          <div class="flex items-center gap-2 rounded-lg border border-ok/40 bg-ok/10 p-2.5 text-sm text-ok">
            <CircleCheck size={16} />{$t('update.restarting')}
          </div>
        {:else if updater.outcome?.action === 'manual'}
          {@const o = updater.outcome}
          <div class="space-y-2 rounded-lg border border-gold/40 bg-gold/5 p-3 text-sm">
            <div class="flex items-center gap-2 text-gold"><CircleCheck size={16} />{$t('update.pkgReady')}</div>
            <div class="truncate font-mono text-xs text-neutral-400" title={o.path}>{o.path}</div>
            <button class="w-full truncate rounded border border-ink-600 bg-black px-2 py-1.5 text-left font-mono text-xs text-neutral-200" onclick={() => copy(o.command)} title={$t('update.copy')}>
              <TerminalSquare size={12} class="mr-1 inline text-gold" />{o.command}
            </button>
            <div class="flex flex-wrap gap-2">
              <button class="btn !py-1.5" onclick={() => api.revealPath(o.path)}><FolderOpen size={14} />{$t('update.openFolder')}</button>
              <button class="btn btn-gold !py-1.5" disabled={pkgBusy} onclick={() => installPackage(o.path)}>
                {#if pkgBusy}<LoaderCircle size={14} class="animate-spin" />{:else}<ShieldCheck size={14} />{/if}{$t('update.pkexec')}
              </button>
            </div>
          </div>
        {/if}

        {#if !info.canInstall}
          <p class="text-xs text-neutral-500">
            {info.kind === 'dev' ? $t('update.devBuild') : info.source === 'api' ? $t('update.noManifest') : $t('update.noAsset')}
          </p>
        {/if}
      {:else}
        <p class="text-sm text-neutral-400">{$t('update.upToDateText')}</p>
      {/if}

      <div class="flex flex-wrap items-center justify-end gap-2 border-t border-ink-600 pt-4">
        {#if info.available}
          {#if !busy && !updater.outcome}
            <button class="btn !py-1.5 text-xs" onclick={() => updater.skip()}><EyeOff size={14} />{$t('update.skip')}</button>
            <button class="btn !py-1.5 text-xs" onclick={() => updater.remindLater()}><Clock size={14} />{$t('update.later')}</button>
          {/if}
          <button class="btn !py-1.5 text-xs" onclick={() => openUrl(info.releaseUrl)}><ExternalLink size={14} />{$t('update.releasePage')}</button>
          {#if info.canInstall && updater.outcome?.action !== 'manual'}
            {#if busy}
              <button class="btn btn-danger !py-1.5" onclick={() => job.cancel()}><X size={14} />{$t('common.cancel')}</button>
            {:else}
              <button class="btn btn-gold !py-1.5" disabled={updater.outcome?.action === 'restart'} onclick={() => updater.install()}>
                <Download size={15} />{$t('update.install')}
              </button>
            {/if}
          {/if}
        {:else}
          <button class="btn btn-gold !py-1.5" onclick={close}>OK</button>
        {/if}
      </div>
    </div>
  </div>
{/if}
