<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { ShieldAlert, ShieldCheck, ScanSearch, FolderOpen, Cpu, Usb } from 'lucide-svelte';
  import { api, type ArbInfo, type FirmwareArb, type EdlStatus } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';

  let { active }: { active: boolean } = $props();

  const dev = $derived(selectedDevice());
  const inFastboot = $derived(!!dev && dev.mode === 'fastboot');

  let arb = $state<ArbInfo | null>(null);
  let reading = $state(false);
  let fw = $state<FirmwareArb | null>(null);
  let edl = $state<EdlStatus | null>(null);
  let lastSerial: string | null = null;

  async function readArb() {
    if (!dev) return;
    reading = true;
    try {
      arb = await api.xiaomiArb(dev.serial);
      pushLog(arb.supported ? 'warn' : 'info', `ARB: ${arb.anti ?? $t('xi.arbNone')}`, 'xiaomi');
    } catch (e) {
      pushLog('error', errText(e), 'xiaomi');
    } finally {
      reading = false;
    }
  }

  // natychmiastowy odczyt ARB po pojawieniu się urządzenia w trybie Fastboot
  $effect(() => {
    const s = inFastboot && dev ? dev.serial : null;
    if (s && s !== lastSerial && active) {
      lastSerial = s;
      readArb();
    }
    if (!s) lastSerial = null;
  });

  $effect(() => {
    if (!active) return;
    let stop = false;
    const tick = async () => {
      try {
        edl = await api.edlStatus();
      } catch {
        /* ignore */
      }
      if (!stop) timer = setTimeout(tick, 2000);
    };
    let timer = setTimeout(tick, 0);
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  });

  async function pickFirmware() {
    const d = await open({ directory: true, multiple: false, title: 'Fastboot ROM (flash_all.bat)' });
    if (typeof d !== 'string') return;
    try {
      fw = await api.xiaomiFirmwareArb(d);
    } catch (e) {
      fw = null;
      pushLog('error', errText(e), 'xiaomi');
    }
  }

  // ocena ryzyka: firmware z niższym ARB niż urządzenie = cegła
  const verdict = $derived.by(() => {
    if (!arb) return null;
    if (arb.anti === null) return { level: 'ok', text: $t('xi.vNoArb') };
    if (fw && fw.anti !== null) {
      return fw.anti >= arb.anti
        ? { level: 'ok', text: $t('xi.vSafe').replace('{fw}', String(fw.anti)).replace('{dev}', String(arb.anti)) }
        : { level: 'bad', text: $t('xi.vBrick').replace('{fw}', String(fw.anti)).replace('{dev}', String(arb.anti)) };
    }
    if (fw && fw.anti === null) return { level: 'warn', text: $t('xi.vFwNoCheck') };
    return arb.anti > 0 ? { level: 'bad', text: $t('xi.vActive') } : { level: 'ok', text: $t('xi.vZero') };
  });
</script>

<div class="grid gap-4 2xl:grid-cols-[1fr_380px]">
  <div class="card space-y-4">
    <div class="flex items-center justify-between">
      <div class="flex items-center gap-2 text-sm font-medium text-gold"><ShieldAlert size={16} />{$t('xi.arbTitle')}</div>
      <button class="btn btn-gold" disabled={!inFastboot || reading} onclick={readArb}>
        <ScanSearch size={15} class={reading ? 'animate-pulse' : ''} />fastboot getvar anti
      </button>
    </div>
    {#if !inFastboot}
      <div class="text-sm text-warn">{$t('xi.needFastboot')}</div>
    {/if}

    {#if arb}
      <div
        class="rounded-xl border-2 p-6 text-center {verdict?.level === 'bad'
          ? 'border-err bg-err/10'
          : verdict?.level === 'warn'
            ? 'border-warn bg-warn/10'
            : 'border-ok bg-ok/10'}"
      >
        <div class="text-xs uppercase tracking-[0.3em] text-neutral-400">ANTI-ROLLBACK</div>
        <div
          class="font-mono text-7xl font-black leading-tight {verdict?.level === 'bad' ? 'text-err' : verdict?.level === 'warn' ? 'text-warn' : 'text-ok'}"
        >
          {arb.anti === null ? $t('xi.arbNoneBig') : `ARB ${arb.anti}`}
        </div>
        <div class="mt-2 text-sm font-bold uppercase {verdict?.level === 'bad' ? 'text-err' : verdict?.level === 'warn' ? 'text-warn' : 'text-ok'}">
          {verdict?.text}
        </div>
      </div>
      <div class="grid gap-2 text-sm sm:grid-cols-3">
        <div><span class="text-neutral-500">product:</span> <span class="font-mono text-neutral-200">{arb.product ?? '—'}</span></div>
        <div><span class="text-neutral-500">unlocked:</span> <span class="font-mono {arb.unlocked ? 'text-warn' : 'text-neutral-200'}">{arb.unlocked === null ? '—' : arb.unlocked ? 'yes' : 'no'}</span></div>
        <div><span class="text-neutral-500">slot:</span> <span class="font-mono text-neutral-200">{arb.slot ?? '—'}</span></div>
      </div>
      <pre class="max-h-24 overflow-y-auto rounded border border-ink-600 bg-black p-2 text-[11px] text-neutral-500">{arb.raw}</pre>
    {/if}

    <div class="space-y-2 border-t border-ink-600 pt-3">
      <div class="flex items-center justify-between gap-2">
        <span class="text-sm text-neutral-300">{$t('xi.fwCompare')}</span>
        <button class="btn !py-1.5 text-xs" onclick={pickFirmware}><FolderOpen size={14} />{$t('xi.pickFw')}</button>
      </div>
      {#if fw}
        <div class="flex flex-wrap gap-x-6 gap-y-1 text-xs">
          <span class="truncate font-mono text-neutral-500" title={fw.folder}>{fw.folder}</span>
          <span>{$t('xi.fwArb')}: <b class="font-mono text-gold">{fw.anti ?? '—'}</b> ({fw.script ?? '?'})</span>
          {#if fw.product}<span>product: <b class="font-mono text-neutral-200">{fw.product}</b></span>{/if}
          {#if fw.product && arb?.product && fw.product !== arb.product}<span class="text-err">{$t('xi.productMismatch')}</span>{/if}
        </div>
      {/if}
    </div>
  </div>

  <div class="card space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-gold"><Cpu size={16} />{$t('xi.edlTitle')}</div>
    {#if edl?.present}
      <div class="rounded-lg border p-4 text-center {edl.ready ? 'border-ok/40 bg-ok/10' : 'border-err/40 bg-err/10'}">
        <div class="text-lg font-bold {edl.ready ? 'text-ok' : 'text-err'}">{edl.ready ? $t('xi.edlReady') : $t('xi.edlNotReady')}</div>
        {#each edl.devices as d, i (i)}<div class="font-mono text-xs text-neutral-400">{d.vid}:{d.pid} {d.product ?? ''}</div>{/each}
        <div class="mt-2 text-xs text-neutral-300">{edl.detail}</div>
      </div>
    {:else}
      <div class="flex items-center gap-2 rounded-lg border border-ink-500 p-4 text-sm text-neutral-500">
        <Usb size={16} class="animate-pulse" />{$t('xi.edlWaiting')}
      </div>
    {/if}
    {#if edl?.otherModes.length}
      <div class="space-y-1 text-xs">
        {#each edl.otherModes as d, i (i)}
          <div class="flex justify-between"><span class="text-neutral-400">{$t('usbkind.' + d.kind)}</span><span class="font-mono text-gold">{d.vid}:{d.pid}</span></div>
        {/each}
      </div>
    {/if}
    <div class="space-y-1 text-xs text-neutral-400">
      <p>• {$t('xi.edlHow1')}</p>
      <p>• {$t('xi.edlHow2')}</p>
      <p>• {$t('xi.edlHow3')}</p>
    </div>
    <p class="flex items-start gap-1.5 text-[11px] text-neutral-600"><ShieldCheck size={12} class="mt-px shrink-0" />{$t('xi.edlNote')}</p>
  </div>
</div>
