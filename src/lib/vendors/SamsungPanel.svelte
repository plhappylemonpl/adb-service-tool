<script lang="ts">
  import { ShieldCheck, ShieldAlert, ScanSearch, Usb, Download, Info } from 'lucide-svelte';
  import { api, type SamsungStatus, type DownloadModeStatus } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';

  let { active }: { active: boolean } = $props();

  const dev = $derived(selectedDevice());
  const adbReady = $derived(!!dev && (dev.mode === 'adb' || dev.mode === 'recovery'));

  let status = $state<SamsungStatus | null>(null);
  let reading = $state(false);
  let dl = $state<DownloadModeStatus | null>(null);

  async function read() {
    if (!dev) return;
    reading = true;
    try {
      status = await api.samsungStatus(dev.serial);
      if (!status.isSamsung) pushLog('warn', $t('sam.notSamsung'), 'samsung');
    } catch (e) {
      pushLog('error', errText(e), 'samsung');
    } finally {
      reading = false;
    }
  }

  // odpytywanie USB co 2 s, tylko gdy panel jest widoczny
  $effect(() => {
    if (!active) return;
    let stop = false;
    const tick = async () => {
      try {
        dl = await api.samsungDownloadMode();
      } catch {
        /* nusb niedostępny - pokażemy brak */
      }
      if (!stop) timer = setTimeout(tick, 2000);
    };
    let timer = setTimeout(tick, 0);
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  });

  const levelColor: Record<string, string> = {
    ok: 'text-ok',
    warn: 'text-warn',
    bad: 'text-err',
    info: 'text-neutral-200'
  };
</script>

<div class="grid gap-4 2xl:grid-cols-[1fr_380px]">
  <div class="card space-y-3">
    <div class="flex items-center justify-between">
      <div class="flex items-center gap-2 text-sm font-medium text-gold"><ShieldCheck size={16} />{$t('sam.knoxTitle')}</div>
      <button class="btn btn-gold" disabled={!adbReady || reading} onclick={read}>
        <ScanSearch size={15} class={reading ? 'animate-pulse' : ''} />{$t('sam.read')}
      </button>
    </div>
    {#if !adbReady}
      <div class="text-sm text-warn">{$t('common.needAdb')}</div>
    {/if}
    {#if status}
      <div class="grid gap-3 sm:grid-cols-3">
        <div class="rounded-lg border p-3 text-center {status.knox === '0x0' ? 'border-ok/40 bg-ok/5' : status.knox === '0x1' ? 'border-err/50 bg-err/10' : 'border-ink-500'}">
          <div class="text-[11px] uppercase tracking-wide text-neutral-500">Knox</div>
          <div class="font-mono text-2xl font-bold {status.knox === '0x0' ? 'text-ok' : status.knox === '0x1' ? 'text-err' : 'text-neutral-400'}">{status.knox ?? '—'}</div>
          <div class="text-[11px] text-neutral-500">{status.knox === '0x0' ? $t('sam.knoxOk') : status.knox === '0x1' ? $t('sam.knoxVoid') : $t('sam.knoxNA')}</div>
        </div>
        <div class="rounded-lg border border-ink-500 p-3 text-center">
          <div class="text-[11px] uppercase tracking-wide text-neutral-500">Binary / bit</div>
          <div class="font-mono text-2xl font-bold text-gold">{status.binary ?? '—'}</div>
          <div class="text-[11px] text-neutral-500">{$t('sam.binaryHint')}</div>
        </div>
        <div class="rounded-lg border border-ink-500 p-3 text-center">
          <div class="text-[11px] uppercase tracking-wide text-neutral-500">KnoxGuard</div>
          <div class="font-mono text-2xl font-bold text-neutral-200">{status.knoxGuard ?? '—'}</div>
          <div class="text-[11px] text-neutral-500">{$t('sam.kgHint')}</div>
        </div>
      </div>
      <div>
        {#each status.rows as r (r.key)}
          <div class="flex justify-between gap-4 border-b border-ink-600/70 py-1.5 text-sm last:border-0">
            <span class="shrink-0 text-neutral-500">{r.label}</span>
            <span class="select-text break-all text-right font-mono {levelColor[r.level]}">{r.value}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="card space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-gold"><Download size={16} />{$t('sam.dlTitle')}</div>
    {#if dl?.present}
      <div class="rounded-lg border border-ok/40 bg-ok/10 p-4 text-center">
        <div class="text-lg font-bold text-ok">{$t('sam.dlDetected')}</div>
        {#each dl.devices as d, i (i)}
          <div class="font-mono text-xs text-neutral-400">{d.vid}:{d.pid} {d.product ?? ''} {d.serial ?? ''}</div>
        {/each}
        {#if dl.driverOk === false}
          <div class="mt-2 text-xs text-err">{$t('sam.dlNoDriver')}</div>
        {:else}
          <div class="mt-2 text-xs text-neutral-400">{$t('sam.dlReady')}</div>
        {/if}
      </div>
    {:else}
      <div class="flex items-center gap-2 rounded-lg border border-ink-500 p-4 text-sm text-neutral-500">
        <Usb size={16} class="animate-pulse" />{$t('sam.dlWaiting')}
      </div>
    {/if}
    <div class="space-y-1.5 text-xs text-neutral-400">
      <div class="flex items-center gap-1.5 font-medium text-neutral-300"><Info size={13} />{$t('sam.dlHow')}</div>
      <p>• {$t('sam.dlHow1')}</p>
      <p>• {$t('sam.dlHow2')}</p>
      <p>• {$t('sam.dlHow3')}</p>
      <p>• {$t('sam.dlHow4')}</p>
    </div>
    <p class="flex items-start gap-1.5 text-[11px] text-neutral-600"><ShieldAlert size={12} class="mt-px shrink-0" />{$t('sam.lokeNote')}</p>
  </div>
</div>
