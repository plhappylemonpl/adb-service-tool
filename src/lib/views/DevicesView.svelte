<script lang="ts">
  import { RefreshCw, Smartphone, Usb, Zap, ShieldAlert, FolderSearch, Radio, Wifi, WifiOff, LoaderCircle } from 'lucide-svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { api, type ConnMode, type Device } from '$lib/api';
  import { app, setDevices, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import { openWifi } from '$lib/components/WifiDialog.svelte';
  import { toast } from '$lib/components/Toast.svelte';

  let scanning = $state(false);
  let disconnecting = $state<string | null>(null);

  const modeStyle: Record<ConnMode, string> = {
    adb: 'bg-ok/15 text-ok',
    recovery: 'bg-gold/15 text-gold',
    sideload: 'bg-gold/15 text-gold',
    fastboot: 'bg-sky-500/15 text-sky-300',
    mtp: 'bg-violet-500/15 text-violet-300',
    unauthorized: 'bg-warn/15 text-warn',
    offline: 'bg-err/15 text-err',
    nopermissions: 'bg-err/15 text-err'
  };

  async function scan() {
    scanning = true;
    try {
      setDevices(await api.scanDevices());
    } catch (e) {
      pushLog('error', errText(e));
    } finally {
      scanning = false;
    }
  }

  async function pickAdb() {
    const f = await open({ multiple: false, directory: false, title: 'adb / adb.exe' });
    if (typeof f !== 'string') return;
    try {
      app.tools = await api.setAdbPath(f);
      pushLog('ok', `adb: ${app.tools.adbPath}`);
      await scan();
    } catch (e) {
      pushLog('error', errText(e));
    }
  }

  async function disconnect(d: Device) {
    disconnecting = d.serial;
    try {
      const out = await api.wifiDisconnect(d.serial);
      pushLog('ok', `Wi-Fi: ${out}`, 'wifi');
      toast('info', $t('wifi.disconnected'), d.serial);
      await scan();
    } catch (e) {
      const msg = errText(e);
      pushLog('error', `Wi-Fi: ${msg}`, 'wifi');
      toast('error', $t('wifi.disconnectFailed'), msg);
    } finally {
      disconnecting = null;
    }
  }

  function select(d: Device) {
    app.selected = d.serial;
  }
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex flex-wrap items-center justify-between gap-3">
    <div>
      <h2 class="view-title">{$t('devices.title')}</h2>
      <p class="flex items-center gap-1.5 text-xs text-neutral-500"><Radio size={12} class="text-ok" />{$t('devices.auto')}</p>
    </div>
    <div class="flex flex-wrap gap-2">
      <button class="btn btn-gold" onclick={() => openWifi('pair')} disabled={!app.tools?.adbPath}>
        <Wifi size={15} />{$t('wifi.button')}
      </button>
      <button class="btn" onclick={scan} disabled={scanning}>
        <RefreshCw size={15} class={scanning ? 'animate-spin' : ''} />{$t('devices.scan')}
      </button>
    </div>
  </div>

  {#if app.devices.length === 0}
    <div class="card flex flex-col items-center gap-3 py-10 text-center">
      <Usb size={40} class="text-neutral-600" />
      <div class="text-sm text-neutral-400">{$t('devices.none')}</div>
      <div class="max-w-md text-xs text-neutral-600">{$t('devices.noneHint')}</div>
      <button class="btn btn-gold mt-2" onclick={() => openWifi('pair')} disabled={!app.tools?.adbPath}>
        <Wifi size={15} />{$t('wifi.button')}
      </button>
    </div>
  {:else}
    <div class="grid gap-3 lg:grid-cols-2">
      {#each app.devices as d (d.serial + d.mode)}
        <div
          class="card group cursor-pointer text-left transition-all hover:border-gold/60 {app.selected === d.serial
            ? '!border-gold shadow-[0_0_0_1px_rgba(201,162,73,0.35)]'
            : ''}"
          role="button"
          tabindex="0"
          onclick={() => select(d)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              select(d);
            }
          }}
        >
          <div class="flex items-start gap-3">
            <div class="rounded-lg bg-ink-600 p-2.5 {app.selected === d.serial ? 'text-gold' : 'text-neutral-400'}">
              {#if d.mode === 'fastboot'}<Zap size={22} />{:else if d.wireless}<Wifi size={22} />{:else}<Smartphone size={22} />{/if}
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center justify-between gap-2">
                <span class="truncate font-medium text-neutral-100">{d.modelHint ?? d.product ?? $t('devices.unknownModel')}</span>
                <div class="flex shrink-0 items-center gap-1.5">
                  <span class="rounded px-2 py-0.5 text-xs font-medium {d.wireless ? 'bg-gold/15 text-gold' : 'bg-ink-600 text-neutral-400'}">
                    {d.wireless ? 'Wi-Fi' : 'USB'}
                  </span>
                  <span class="rounded px-2 py-0.5 text-xs font-medium {modeStyle[d.mode]}">{$t('mode.' + d.mode)}</span>
                </div>
              </div>
              <div class="mt-0.5 truncate font-mono text-xs text-neutral-500">{d.serial}</div>
              {#if d.mode === 'unauthorized'}
                <div class="mt-2 flex items-start gap-1.5 text-xs text-warn"><ShieldAlert size={13} class="mt-px shrink-0" />{$t('devices.unauthorized')}</div>
              {:else if d.mode === 'offline'}
                <div class="mt-2 text-xs text-err">{d.wireless ? $t('wifi.offlineHint') : $t('devices.offline')}</div>
              {:else if d.mode === 'nopermissions'}
                <div class="mt-2 text-xs text-err">{$t('devices.noPermissions')}</div>
              {:else if d.mode === 'mtp'}
                <div class="mt-2 text-xs text-violet-300">{$t('devices.mtpHint')}</div>
              {/if}

              {#if d.wireless || d.mode === 'adb'}
                <div class="mt-3 flex flex-wrap gap-2 border-t border-ink-600 pt-3">
                  {#if d.wireless}
                    <button
                      class="btn btn-danger !px-3 !py-1.5 text-xs"
                      onclick={(e) => {
                        e.stopPropagation();
                        disconnect(d);
                      }}
                      disabled={disconnecting === d.serial}
                    >
                      {#if disconnecting === d.serial}<LoaderCircle size={14} class="animate-spin" />{:else}<WifiOff size={14} />{/if}
                      {$t('wifi.disconnect')}
                    </button>
                  {:else}
                    <button
                      class="btn !px-3 !py-1.5 text-xs"
                      onclick={(e) => {
                        e.stopPropagation();
                        openWifi('tcpip', d.serial);
                      }}
                    >
                      <Wifi size={14} />{$t('wifi.switchTcp')}
                    </button>
                  {/if}
                </div>
              {/if}
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <div class="card space-y-2 text-xs">
    <div class="text-sm font-medium text-neutral-300">{$t('tools.title')}</div>
    <div class="flex flex-wrap items-center gap-x-6 gap-y-1">
      <span class="text-neutral-500">adb:</span>
      <span class="font-mono {app.tools?.adbPath ? 'text-neutral-300' : 'text-err'}">{app.tools?.adbPath ?? $t('tools.missingAdb')}</span>
    </div>
    {#if app.tools?.adbVersion}
      <div class="font-mono text-neutral-500">{app.tools.adbVersion}</div>
    {/if}
    <div class="flex flex-wrap items-center gap-x-6 gap-y-1">
      <span class="text-neutral-500">fastboot:</span>
      <span class="font-mono {app.tools?.fastbootPath ? 'text-neutral-300' : 'text-warn'}">{app.tools?.fastbootPath ?? $t('tools.missingFastboot')}</span>
    </div>
    <button class="btn mt-1 !py-1.5 text-xs" onclick={pickAdb}><FolderSearch size={14} />{$t('tools.pick')}</button>
  </div>
</section>
