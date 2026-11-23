<script lang="ts">
  import { onMount } from 'svelte';
  import { Smartphone, Info, Download, Upload, Package, Power, Wrench, Languages, CircleAlert, MonitorSmartphone, Factory, Activity, ScrollText, Cable, Rocket, FileCode2, Wifi, Contact, Sparkles } from 'lucide-svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { updater } from '$lib/updater.svelte';
  import UpdateDialog from '$lib/components/UpdateDialog.svelte';
  import { api } from '$lib/api';
  import { app, setDevices, pushLog, selectedDevice } from '$lib/stores.svelte';
  import { t, toggleLang, lang } from '$lib/i18n';
  import { errText } from '$lib/format';
  import LogPanel from '$lib/components/LogPanel.svelte';
  import Confirm from '$lib/components/Confirm.svelte';
  import Toast from '$lib/components/Toast.svelte';
  import WifiDialog, { openWifi } from '$lib/components/WifiDialog.svelte';
  import DevicesView from '$lib/views/DevicesView.svelte';
  import InfoView from '$lib/views/InfoView.svelte';
  import PullView from '$lib/views/PullView.svelte';
  import PushView from '$lib/views/PushView.svelte';
  import BackupView from '$lib/views/BackupView.svelte';
  import AppsView from '$lib/views/AppsView.svelte';
  import RebootView from '$lib/views/RebootView.svelte';
  import AdvancedView from '$lib/views/AdvancedView.svelte';
  import MirrorView from '$lib/views/MirrorView.svelte';
  import VendorsView from '$lib/views/VendorsView.svelte';
  import DiagnosticsView from '$lib/views/DiagnosticsView.svelte';
  import LogcatView from '$lib/views/LogcatView.svelte';
  import DriversView from '$lib/views/DriversView.svelte';
  import EnterpriseView from '$lib/views/EnterpriseView.svelte';
  import DevToolsView from '$lib/views/DevToolsView.svelte';

  type Tab =
    | 'devices' | 'info' | 'pull' | 'push' | 'backup' | 'apps' | 'mirror'
    | 'vendors' | 'diag' | 'logcat' | 'drivers' | 'enterprise' | 'devtools' | 'reboot' | 'advanced';
  let tab = $state<Tab>('devices');
  // widoki montujemy przy pierwszym otwarciu i NIE odmontowujemy - zadania w tle
  // (pull/push/apps) i ich postęp przetrwają przełączanie zakładek
  let opened = $state<Record<string, boolean>>({ devices: true });

  // grupy nawigacji (nagłówek sekcji przed pierwszym elementem grupy)
  const navItems: { id: Tab; icon: typeof Smartphone; key: string; section?: string }[] = [
    { id: 'devices', icon: Smartphone, key: 'nav.devices', section: 'nav.sBasic' },
    { id: 'info', icon: Info, key: 'nav.info' },
    { id: 'pull', icon: Download, key: 'nav.pull' },
    { id: 'push', icon: Upload, key: 'nav.push' },
    { id: 'backup', icon: Contact, key: 'nav.backup' },
    { id: 'apps', icon: Package, key: 'nav.apps' },
    { id: 'mirror', icon: MonitorSmartphone, key: 'nav.mirror' },
    { id: 'vendors', icon: Factory, key: 'nav.vendors', section: 'nav.sService' },
    { id: 'diag', icon: Activity, key: 'nav.diag' },
    { id: 'logcat', icon: ScrollText, key: 'nav.logcat' },
    { id: 'drivers', icon: Cable, key: 'nav.drivers' },
    { id: 'enterprise', icon: Rocket, key: 'nav.enterprise' },
    { id: 'devtools', icon: FileCode2, key: 'nav.devtools' },
    { id: 'reboot', icon: Power, key: 'nav.reboot', section: 'nav.sSystem' },
    { id: 'advanced', icon: Wrench, key: 'nav.advanced' }
  ];

  function go(id: Tab) {
    tab = id;
    opened[id] = true;
  }

  const dev = $derived(selectedDevice());
  let version = $state('');

  onMount(() => {
    let unlisten: (() => void) | undefined;
    getVersion().then((v) => (version = v)).catch(() => {});
    // sprawdzenie aktualizacji w tle (krótki timeout w backendzie, błędy po cichu)
    updater.scheduleStartupCheck(5000);
    (async () => {
      try {
        app.tools = await api.locateTools();
        if (!app.tools.adbPath) pushLog('error', $t('tools.missingAdb'));
      } catch (e) {
        pushLog('error', errText(e));
      }
      try {
        setDevices(await api.scanDevices());
      } catch {
        /* brak adb - baner wyżej */
      }
      unlisten = await api.onDevicesChanged((list) => setDevices(list));
    })();
    return () => unlisten?.();
  });
</script>

<div class="flex h-screen flex-col overflow-hidden bg-ink-900 text-neutral-200">
  <!-- Header -->
  <header class="flex items-center justify-between border-b border-ink-500 bg-ink-800 px-5 py-2.5">
    <div class="flex items-center gap-3">
      <div class="flex h-9 w-9 items-center justify-center rounded-lg border border-gold/40 bg-black text-xl font-black text-gold shadow-[0_0_12px_rgba(201,162,73,0.25)]">D</div>
      <div>
        <h1 class="text-[15px] font-semibold leading-tight text-neutral-100">{$t('app.title')}</h1>
        <p class="text-[11px] text-neutral-500">{$t('app.subtitle')}</p>
      </div>
    </div>
    <div class="flex items-center gap-3">
      {#if dev}
        <div class="hidden items-center gap-2 rounded-lg border border-ink-500 bg-ink-900 px-3 py-1.5 text-xs md:flex">
          <span class="h-2 w-2 rounded-full {dev.mode === 'adb' ? 'bg-ok' : dev.mode === 'unauthorized' || dev.mode === 'offline' ? 'bg-err' : 'bg-gold'}"></span>
          {#if dev.wireless}<Wifi size={13} class="text-gold" />{/if}
          <span class="max-w-[180px] truncate text-neutral-300">{dev.modelHint ?? dev.serial}</span>
          <span class="text-neutral-500">{$t('mode.' + dev.mode)}</span>
        </div>
      {/if}
      {#if updater.pending}
        <button
          class="flex items-center gap-1.5 rounded-full border border-gold/50 bg-gold/10 px-3 py-1 text-xs text-gold transition hover:bg-gold/20"
          onclick={() => (updater.open = true)}
          title={$t('update.title')}
        >
          <Sparkles size={13} />{$t('update.badge').replace('{v}', updater.info?.latest ?? '')}
        </button>
      {/if}
      <button class="btn btn-gold !py-1.5" onclick={() => openWifi('pair')} disabled={!app.tools?.adbPath} title={$t('wifi.subtitle')}>
        <Wifi size={15} />{$t('wifi.button')}
      </button>
      <button class="btn !py-1.5" onclick={toggleLang} title="PL / EN"><Languages size={15} />{$lang.toUpperCase()}</button>
    </div>
  </header>

  {#if app.tools && !app.tools.adbPath}
    <div class="flex items-center gap-2 border-b border-err/30 bg-err/10 px-5 py-2 text-xs text-err">
      <CircleAlert size={14} />{$t('tools.missingAdb')}
      <button class="ml-auto underline" onclick={() => go('devices')}>{$t('tools.pick')}</button>
    </div>
  {/if}

  <div class="flex min-h-0 flex-1">
    <!-- Sidebar -->
    <nav class="flex w-56 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-ink-500 bg-ink-800 p-3">
      {#each navItems as it (it.id)}
        {#if it.section}
          <div class="px-3 pb-1 pt-3 text-[10px] font-semibold uppercase tracking-[0.18em] text-neutral-600 first:pt-0">{$t(it.section)}</div>
        {/if}
        <button class="tab {tab === it.id ? 'tab-active' : ''}" onclick={() => go(it.id)}>
          <it.icon size={17} />
          {$t(it.key)}
        </button>
      {/each}

      <div class="mt-auto space-y-2">
        {#if app.devices.length > 1}
          <select class="input text-xs" bind:value={app.selected}>
            {#each app.devices as d (d.serial + d.mode)}
              <option value={d.serial}>{d.modelHint ?? d.serial} ({d.mode})</option>
            {/each}
          </select>
        {/if}
        <div class="rounded-lg border border-ink-500 bg-ink-900 p-2.5 text-xs">
          {#if dev}
            <div class="text-neutral-500">{$t('devices.selected')}</div>
            <div class="truncate font-mono text-gold" title={dev.serial}>{dev.serial}</div>
          {:else}
            <div class="text-neutral-500">{$t('devices.noneShort')}</div>
          {/if}
        </div>
        <div class="text-center text-[10px] text-neutral-600">Drapco • v{version || '1.0.0'}</div>
      </div>
    </nav>

    <!-- Main -->
    <main class="min-w-0 flex-1 overflow-y-auto p-5">
      {#if opened.devices}<div class:hidden={tab !== 'devices'}><DevicesView /></div>{/if}
      {#if opened.info}<div class:hidden={tab !== 'info'}><InfoView /></div>{/if}
      {#if opened.pull}<div class:hidden={tab !== 'pull'}><PullView /></div>{/if}
      {#if opened.push}<div class:hidden={tab !== 'push'}><PushView /></div>{/if}
      {#if opened.backup}<div class:hidden={tab !== 'backup'}><BackupView /></div>{/if}
      {#if opened.apps}<div class:hidden={tab !== 'apps'}><AppsView /></div>{/if}
      {#if opened.mirror}<div class:hidden={tab !== 'mirror'}><MirrorView /></div>{/if}
      {#if opened.vendors}<div class:hidden={tab !== 'vendors'}><VendorsView active={tab === 'vendors'} /></div>{/if}
      {#if opened.diag}<div class:hidden={tab !== 'diag'}><DiagnosticsView active={tab === 'diag'} /></div>{/if}
      {#if opened.logcat}<div class:hidden={tab !== 'logcat'}><LogcatView /></div>{/if}
      {#if opened.drivers}<div class:hidden={tab !== 'drivers'}><DriversView /></div>{/if}
      {#if opened.enterprise}<div class:hidden={tab !== 'enterprise'}><EnterpriseView /></div>{/if}
      {#if opened.devtools}<div class:hidden={tab !== 'devtools'}><DevToolsView /></div>{/if}
      {#if opened.reboot}<div class:hidden={tab !== 'reboot'}><RebootView /></div>{/if}
      {#if opened.advanced}<div class:hidden={tab !== 'advanced'}><AdvancedView /></div>{/if}
    </main>

    <LogPanel />
  </div>
</div>

<Confirm />
<WifiDialog />
<UpdateDialog />
<Toast />
