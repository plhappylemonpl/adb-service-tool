<script lang="ts">
  import { Info, Copy, Cpu, BatteryMedium, HardDrive, ShieldCheck, Smartphone } from 'lucide-svelte';
  import { api, type DeviceInfo } from '$lib/api';
  import { app, selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { humanKb, errText } from '$lib/format';

  let info = $state<DeviceInfo | null>(null);
  let loading = $state(false);
  let lastSerial = $state<string | null>(null);

  const dev = $derived(selectedDevice());
  const readable = $derived(dev && ['adb', 'recovery', 'fastboot', 'mtp'].includes(dev.mode));

  // automatyczny odczyt po wybraniu innego urządzenia
  $effect(() => {
    const d = dev;
    if (d && readable && d.serial !== lastSerial && !loading) {
      lastSerial = d.serial;
      read();
    }
  });

  async function read() {
    const d = dev;
    if (!d) return;
    loading = true;
    try {
      info = await api.deviceInfo(d.serial, d.mode);
      pushLog('ok', `${$t('info.read')}: ${info.manufacturer ?? ''} ${info.model ?? info.device ?? ''}`.trim());
    } catch (e) {
      pushLog('error', errText(e));
    } finally {
      loading = false;
    }
  }

  function v(x: unknown): string {
    return x === null || x === undefined || x === '' ? '' : String(x);
  }

  const groups = $derived.by(() => {
    const i = info;
    if (!i) return [];
    const yesNo = (b: boolean | null | undefined) => (b == null ? '' : b ? $t('common.yes') : $t('common.no'));
    return [
      {
        title: 'info.gDevice',
        icon: Smartphone,
        rows: [
          ['info.manufacturer', v(i.manufacturer)],
          ['info.brand', v(i.brand)],
          ['info.model', v(i.model)],
          ['info.device', v(i.device)],
          ['info.serial', v(i.serialNo)],
          ['info.imei', v(i.imei) || (i.source === 'adb' ? $t('info.imeiHint') : '')],
          ['info.csc', v(i.csc)]
        ]
      },
      {
        title: 'info.gSoftware',
        icon: Info,
        rows: [
          ['info.android', i.androidVersion ? `${i.androidVersion} (SDK ${v(i.sdk)})` : ''],
          ['info.build', v(i.buildId)],
          ['info.patch', v(i.securityPatch)],
          ['info.baseband', v(i.baseband)],
          ['info.bootloader', v(i.bootloader)],
          ['info.kernel', v(i.kernel)],
          ['info.fingerprint', v(i.fingerprint)]
        ]
      },
      {
        title: 'info.gHardware',
        icon: Cpu,
        rows: [
          ['info.soc', v(i.soc)],
          ['info.hardware', v(i.hardware)],
          ['info.abi', v(i.cpuAbi)],
          ['info.ram', i.ramKb ? humanKb(i.ramKb) : ''],
          ['info.storage', i.storageTotalKb ? `${humanKb(i.storageUsedKb)} / ${humanKb(i.storageTotalKb)}` : ''],
          ['info.resolution', v(i.resolution)]
        ]
      },
      {
        title: 'info.gBattery',
        icon: BatteryMedium,
        rows: [
          ['info.batLevel', i.batteryLevel != null ? `${i.batteryLevel}%` : ''],
          ['info.batStatus', i.batteryStatus ? $t('bat.' + i.batteryStatus) : ''],
          ['info.batHealth', i.batteryHealth ? $t('bat.' + i.batteryHealth) : ''],
          ['info.batTemp', i.batteryTempC != null ? `${i.batteryTempC.toFixed(1)} °C` : ''],
          ['info.batVoltage', i.batteryVoltageMv != null ? `${i.batteryVoltageMv} mV` : '']
        ]
      },
      {
        title: 'info.gSecurity',
        icon: ShieldCheck,
        rows: [
          ['info.blLocked', yesNo(i.bootloaderLocked)],
          ['info.verifiedBoot', v(i.verifiedBoot)],
          ['info.slot', v(i.slot)]
        ]
      }
    ]
      .map((g) => ({ ...g, rows: g.rows.filter((r) => r[1]) }))
      .filter((g) => g.rows.length > 0);
  });

  async function copyAll() {
    if (!info) return;
    const lines: string[] = [];
    for (const g of groups) for (const r of g.rows) lines.push(`${$t(r[0])}: ${r[1]}`);
    for (const [k, val] of info.extra) lines.push(`${k}: ${val}`);
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      pushLog('ok', $t('common.copied'));
    } catch {
      /* brak schowka */
    }
  }
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex items-center justify-between">
    <h2 class="view-title">{$t('info.title')}</h2>
    <div class="flex gap-2">
      <button class="btn" disabled={!info} onclick={copyAll}><Copy size={15} />{$t('common.copy')}</button>
      <button class="btn btn-gold" disabled={!readable || loading} onclick={read}>
        <Info size={15} class={loading ? 'animate-pulse' : ''} />{$t('info.read')}
      </button>
    </div>
  </div>

  {#if !dev}
    <div class="card text-sm text-neutral-500">{$t('devices.none')}</div>
  {:else if !readable}
    <div class="card text-sm text-warn">{$t('info.notReadable')}</div>
  {:else if loading && !info}
    <div class="card animate-pulse text-sm text-neutral-500">{$t('common.loading')}</div>
  {:else if info}
    <div class="grid gap-4 xl:grid-cols-2">
      {#each groups as g (g.title)}
        <div class="card">
          <div class="mb-2 flex items-center gap-2 text-sm font-medium text-gold"><g.icon size={15} />{$t(g.title)}</div>
          {#each g.rows as row (row[0])}
            <div class="flex justify-between gap-4 border-b border-ink-600/70 py-1.5 text-sm last:border-0">
              <span class="shrink-0 text-neutral-500">{$t(row[0])}</span>
              <span class="select-text break-all text-right font-mono text-neutral-200">{row[1]}</span>
            </div>
          {/each}
        </div>
      {/each}
    </div>

    {#if info.extra.length}
      <details class="card">
        <summary class="cursor-pointer text-sm text-neutral-300"><HardDrive size={14} class="mr-1 inline" />{$t('info.extra')} ({info.extra.length})</summary>
        <div class="mt-2 grid gap-x-6 sm:grid-cols-2">
          {#each info.extra as row (row[0])}
            <div class="flex justify-between gap-4 border-b border-ink-600/70 py-1 text-xs">
              <span class="text-neutral-500">{row[0]}</span>
              <span class="select-text break-all text-right font-mono text-neutral-300">{row[1]}</span>
            </div>
          {/each}
        </div>
      </details>
    {/if}
  {/if}
</section>
