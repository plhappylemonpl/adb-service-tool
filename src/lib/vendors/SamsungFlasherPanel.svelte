<script lang="ts">
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { open as openUrl } from '@tauri-apps/plugin-shell';
  import {
    Download, HardDriveDownload, FileCode2, CircleCheck, CircleX, TriangleAlert, Zap, ScanSearch,
    Usb, ExternalLink, FolderOpen, RotateCcw
  } from 'lucide-svelte';
  import { api, type FirmwareSet, type HeimdallStatus, type DownloadProbe, type TarRole } from '$lib/api';
  import { selectedDevice, Job, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText, humanBytes, basename } from '$lib/format';
  import ProgressCard from '$lib/components/ProgressCard.svelte';
  import { confirmDialog } from '$lib/components/Confirm.svelte';

  let { active = true }: { active?: boolean } = $props();

  const pitJob = new Job('pit');
  const flashJob = new Job('odin');

  let heimdall = $state<HeimdallStatus | null>(null);
  let probe = $state<DownloadProbe | null>(null);

  // wybrane paczki wg roli
  type Slot = 'bl' | 'ap' | 'cp' | 'csc';
  let slots = $state<Record<Slot, string | null>>({ bl: null, ap: null, cp: null, csc: null });
  let set = $state<FirmwareSet | null>(null);
  let inspecting = $state(false);

  let verifyMd5 = $state(true);
  let autoReboot = $state(true);
  let repartition = $state(false);
  let pitPath = $state<string | null>(null);

  async function refresh() {
    try {
      heimdall = await api.heimdallStatus();
    } catch (e) {
      pushLog('error', errText(e), 'odin');
    }
  }

  // sonduj Download Mode co 2 s gdy panel widoczny
  $effect(() => {
    if (!active) return;
    refresh();
    let stop = false;
    const tick = async () => {
      try {
        // w trakcie flashowania Heimdall ma wyłączność na USB - nie sondujemy
        if (!flashJob.running) probe = await api.downloadProbe();
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

  const slotLabels: Record<Slot, string> = { bl: 'BL (bootloader)', ap: 'AP (PDA)', cp: 'CP (modem)', csc: 'CSC' };
  const roleOf: Record<TarRole, Slot | null> = {
    bl: 'bl', ap: 'ap', cp: 'cp', csc: 'csc', homeCsc: 'csc', userdata: null, unknown: null
  };

  async function pickSlot(slot: Slot) {
    const r = await open({ multiple: false, filters: [{ name: 'Samsung firmware', extensions: ['md5', 'tar'] }] });
    if (typeof r !== 'string') return;
    slots[slot] = r;
    await inspect();
  }

  async function inspect() {
    const paths = Object.values(slots).filter((p): p is string => !!p);
    if (!paths.length) {
      set = null;
      return;
    }
    inspecting = true;
    try {
      set = await api.firmwareInspect(paths);
      // ostrzeż gdy rola pliku nie pasuje do slotu
      for (const part of set.parts) {
        const want = roleOf[part.role];
        pushLog('info', `${part.filename}: ${part.role.toUpperCase()}, ${part.images.length} obrazów, ${humanBytes(part.totalSize)}`, 'odin');
        if (!want) pushLog('warn', `${part.filename}: nierozpoznana rola / unknown role`, 'odin');
      }
    } catch (e) {
      set = null;
      pushLog('error', errText(e), 'odin');
    } finally {
      inspecting = false;
    }
  }

  function clearSlot(slot: Slot) {
    slots[slot] = null;
    inspect();
  }

  async function readPit() {
    let savePath: string | null = null;
    const r = await save({ defaultPath: 'device.pit', filters: [{ name: 'PIT', extensions: ['pit'] }] });
    if (typeof r === 'string') savePath = r;
    await pitJob.start((on) => api.readPit(savePath, on));
  }

  async function pickPit() {
    const r = await open({ multiple: false, filters: [{ name: 'PIT', extensions: ['pit'] }] });
    if (typeof r === 'string') pitPath = r;
  }

  const canFlash = $derived(!!heimdall?.path && !!probe?.present && Object.values(slots).some((p) => !!p));

  async function flash() {
    const paths = Object.values(slots).filter((p): p is string => !!p);
    const ok = await confirmDialog({
      title: $t('odin.confirmTitle'),
      message:
        $t('odin.confirmText') +
        (repartition ? '\n\n' + $t('odin.confirmRepart') : '') +
        (set?.hasUserdata ? '\n\n' + $t('odin.confirmData') : ''),
      items: paths.map(basename),
      danger: true,
      confirmLabel: $t('odin.flash')
    });
    if (!ok) return;
    await flashJob.start((on) =>
      api.samsungFlash({ parts: paths, verifyMd5, autoReboot, repartition, pitPath, acknowledged: true }, on)
    );
  }

  const slotArr: Slot[] = ['bl', 'ap', 'cp', 'csc'];
</script>

<div class="space-y-4">
  <!-- status: Download Mode + Heimdall -->
  <div class="grid gap-3 md:grid-cols-2">
    <div class="card flex items-center gap-3 {probe?.present ? 'border-ok/40' : ''}">
      <Usb size={20} class={probe?.present ? 'text-ok' : 'text-neutral-500'} />
      <div class="min-w-0 flex-1 text-sm">
        {#if probe?.present}
          <div class="font-medium text-ok">{$t('odin.dlConnected')}</div>
          <div class="truncate font-mono text-xs text-neutral-500">
            {#if probe.busy || flashJob.running}{$t('odin.usbBusy')}{/if}
            {probe.devices[0]?.vid}:{probe.devices[0]?.pid} {probe.devices[0]?.product ?? ''}
            {#if probe.accessible === false}<span class="text-err"> · {$t('drivers.noAccess')}</span>{/if}
          </div>
        {:else}
          <div class="text-neutral-400">{$t('odin.dlWaiting')}</div>
        {/if}
      </div>
      <button class="btn !py-1.5 text-xs" disabled={!probe?.present || pitJob.running || flashJob.running} onclick={readPit}>
        <ScanSearch size={14} />{$t('odin.readPit')}
      </button>
    </div>

    <div class="card flex items-center gap-3 {heimdall?.path ? 'border-ok/30' : 'border-warn/40'}">
      <HardDriveDownload size={20} class={heimdall?.path ? 'text-ok' : 'text-warn'} />
      <div class="min-w-0 flex-1 text-sm">
        {#if heimdall?.path}
          <div class="text-neutral-200">Heimdall {heimdall.version ?? ''}</div>
          <div class="truncate font-mono text-xs text-neutral-500">{heimdall.path}</div>
        {:else}
          <div class="text-warn">{$t('odin.noHeimdall')}</div>
          {#if heimdall?.installHint}<div class="font-mono text-[11px] text-neutral-500">{heimdall.installHint}</div>{/if}
        {/if}
      </div>
    </div>
  </div>

  <ProgressCard job={pitJob} />

  <!-- wybór paczek (flex + gap zamiast space-y: marginesy dzieci nie są nadpisywane) -->
  <div class="card flex flex-col gap-4">
    <div class="flex items-center gap-2 text-sm font-medium text-gold"><FileCode2 size={16} />{$t('odin.firmware')}</div>
    <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      {#each slotArr as slot (slot)}
        {@const part = set?.parts.find((p) => roleOf[p.role] === slot)}
        <div class="rounded-lg border p-3 {slots[slot] ? 'border-gold/50 bg-gold/5' : 'border-ink-500'}">
          <div class="mb-1 flex items-center justify-between">
            <span class="text-xs font-semibold text-neutral-300">{slotLabels[slot]}</span>
            {#if slots[slot]}<button class="text-neutral-500 hover:text-err" onclick={() => clearSlot(slot)}><CircleX size={14} /></button>{/if}
          </div>
          {#if slots[slot]}
            <div class="truncate font-mono text-[11px] text-neutral-400" title={slots[slot]}>{basename(slots[slot]!)}</div>
            {#if part}
              <div class="mt-1 flex items-center gap-2 text-[11px]">
                <span class="text-neutral-500">{part.images.length} obr. · {humanBytes(part.totalSize)}</span>
                {#if part.hasMd5}<span class="badge bg-ok/10 text-ok">MD5</span>{:else}<span class="badge bg-warn/15 text-warn">brak MD5</span>{/if}
              </div>
            {/if}
          {:else}
            <button class="btn w-full justify-center !py-1.5 text-xs" onclick={() => pickSlot(slot)}><FolderOpen size={13} />{$t('odin.pick')}</button>
          {/if}
        </div>
      {/each}
    </div>

    <!-- opcje flashowania: własny rząd flex, etykiety nie łamią się w środku -->
    <div class="flex flex-row flex-wrap items-center gap-x-6 gap-y-3 border-t border-ink-600 pt-4">
      <label class="toggle whitespace-nowrap">
        <input type="checkbox" bind:checked={verifyMd5} /><span></span>{$t('odin.verify')}
      </label>
      <label class="toggle whitespace-nowrap">
        <input type="checkbox" bind:checked={autoReboot} /><span></span>{$t('odin.autoReboot')}
      </label>
      <label class="toggle whitespace-nowrap">
        <input type="checkbox" bind:checked={repartition} /><span></span><span class="text-err">{$t('odin.repartition')}</span>
      </label>
      {#if repartition}
        <button
          class="btn shrink-0 whitespace-nowrap !py-1 text-xs {pitPath ? 'btn-gold' : ''}"
          title={pitPath ?? ''}
          onclick={pickPit}
        >
          <FolderOpen size={13} />{pitPath ? basename(pitPath) : 'PIT…'}
        </button>
      {/if}
    </div>

    {#if repartition}
      <div class="flex items-start gap-2 rounded-lg border border-err/40 bg-err/10 p-2.5 text-xs text-err">
        <TriangleAlert size={14} class="mt-px shrink-0" />{$t('odin.repartWarn')}
      </div>
    {/if}

    <!-- główny przycisk: osobny blok, wyraźny odstęp od opcji -->
    <div class="mt-2 flex flex-col gap-2 border-t border-ink-600 pt-5">
      <button
        class="btn btn-danger-solid w-full justify-center py-3.5 text-base font-semibold"
        disabled={!canFlash || flashJob.running}
        onclick={flash}
      >
        <Zap size={18} />{$t('odin.flash')}
      </button>
      {#if !heimdall?.path}<p class="text-center text-[11px] text-warn">{$t('odin.needHeimdall')}</p>{/if}
    </div>
  </div>

  <div class="flex items-start gap-2 rounded-lg border border-ink-500 bg-ink-800/60 p-3 text-[11px] text-neutral-500">
    <TriangleAlert size={13} class="mt-px shrink-0 text-gold" />
    {$t('odin.engineNote')}
    <button class="ml-1 inline-flex items-center gap-1 text-gold hover:underline" onclick={() => openUrl('https://glassechidna.com.au/heimdall/')}>
      <ExternalLink size={11} />heimdall
    </button>
  </div>

  <ProgressCard job={flashJob} />
</div>
