<script lang="ts">
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import { open } from '@tauri-apps/plugin-dialog';
  import {
    RefreshCw, Search, Download, Trash2, Trash, PowerOff, Power, Undo2, PackagePlus, FolderOpen,
    ShieldAlert, CircleCheck, CircleX, CheckSquare, Square as SquareIcon, Ban, Eraser, EllipsisVertical
  } from 'lucide-svelte';
  import { api, ACTION_MIN_SDK, type AppAction, type AppDetails, type AppEntry, type AppsLoadEvent } from '$lib/api';
  import { selectedDevice, Job, pushLog } from '$lib/stores.svelte';
  import { t, lang, apkLang } from '$lib/i18n';
  import { errText, humanBytes } from '$lib/format';
  import AppIcon from '$lib/components/AppIcon.svelte';
  import AppsLoader from '$lib/components/AppsLoader.svelte';
  import ProgressCard from '$lib/components/ProgressCard.svelte';
  import { confirmDialog } from '$lib/components/Confirm.svelte';

  type Filter = 'user' | 'system' | 'disabled' | 'removed' | 'all';
  type Stage = 'idle' | 'list' | 'details' | 'done' | 'cancelled' | 'error';

  const job = new Job('apps');
  let apps = $state<AppEntry[]>([]);
  let sdk = $state(0);
  let filter = $state<Filter>('user');
  let query = $state('');
  const checked = new SvelteSet<string>();
  const details = new SvelteMap<string, AppDetails>();
  const status = new SvelteMap<string, { ok: boolean; msg: string }>();
  let installDowngrade = $state(false);
  let installGrant = $state(false);

  // --- stan ładowania (pasek nad listą) ---
  let stage = $state<Stage>('idle');
  let loadDone = $state(0);
  let loadTotal = $state(0);
  let loadCurrent = $state<string | null>(null);
  let loadFailed = $state(0);
  let loadId: string | null = null;
  let gen = 0;
  let loadedFor = $state<string | null>(null);

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && dev.mode === 'adb');
  const loadingList = $derived(stage === 'list');
  const loadingDetails = $derived(stage === 'details');

  $effect(() => {
    const serial = ready && dev ? dev.serial : null;
    if (serial && loadedFor !== serial) {
      loadedFor = serial;
      load();
    }
    if (!serial && loadId) stopLoad();
  });

  async function stopLoad() {
    if (loadId) {
      const id = loadId;
      loadId = null;
      await api.cancelJob(id).catch(() => {});
    }
  }

  function onLoadEvent(myGen: number, e: AppsLoadEvent) {
    if (myGen !== gen) return; // zdarzenia z poprzedniego (anulowanego) ładowania
    switch (e.kind) {
      case 'stage':
        stage = e.stage;
        break;
      case 'list':
        apps = e.apps;
        sdk = e.sdk;
        pushLog('info', `${$t('apps.loaded')}: ${e.apps.length} (SDK ${e.sdk})`, 'apps');
        break;
      case 'details':
        details.set(e.details.package, e.details);
        break;
      case 'progress':
        loadDone = e.done;
        loadTotal = e.total;
        loadCurrent = e.current;
        break;
      case 'finished':
        stage = e.cancelled ? 'cancelled' : 'done';
        loadFailed = e.failed;
        loadId = null;
        loadCurrent = null;
        break;
      case 'error':
        stage = 'error';
        pushLog('error', e.message, 'apps');
        break;
    }
  }

  async function load() {
    if (!dev) return;
    await stopLoad();
    const myGen = ++gen;
    stage = 'list';
    loadDone = 0;
    loadTotal = 0;
    loadFailed = 0;
    loadCurrent = null;
    apps = [];
    checked.clear();
    status.clear();
    details.clear();
    try {
      loadId = await api.loadApps(dev.serial, apkLang($lang), (e) => onLoadEvent(myGen, e));
    } catch (e) {
      stage = 'error';
      pushLog('error', errText(e), 'apps');
    }
  }

  const counts = $derived({
    user: apps.filter((a) => !a.system && a.installed).length,
    system: apps.filter((a) => a.system && a.installed).length,
    disabled: apps.filter((a) => !a.enabled && a.installed).length,
    removed: apps.filter((a) => !a.installed).length,
    all: apps.length
  });

  const visible = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return apps.filter((a) => {
      const okF =
        filter === 'all' ||
        (filter === 'user' && !a.system && a.installed) ||
        (filter === 'system' && a.system && a.installed) ||
        (filter === 'disabled' && !a.enabled && a.installed) ||
        (filter === 'removed' && !a.installed);
      if (!okF) return false;
      if (!q) return true;
      const label = details.get(a.package)?.label?.toLowerCase() ?? '';
      return a.package.toLowerCase().includes(q) || label.includes(q);
    });
  });

  const allVisibleChecked = $derived(visible.length > 0 && visible.every((a) => checked.has(a.package)));

  function toggleAll() {
    if (allVisibleChecked) visible.forEach((a) => checked.delete(a.package));
    else visible.forEach((a) => checked.add(a.package));
  }

  function nameOf(a: AppEntry): string {
    const l = details.get(a.package)?.label;
    return l ? `${l} (${a.package})` : a.package;
  }

  function supported(a: AppAction): boolean {
    return sdk === 0 || sdk >= ACTION_MIN_SDK[a];
  }

  function applyLocal(pkg: string, action: AppAction) {
    const idx = apps.findIndex((x) => x.package === pkg);
    if (idx < 0) return;
    const a = apps[idx];
    if (action === 'uninstall') a.installed = false;
    if (action === 'uninstallFull') apps.splice(idx, 1);
    if (action === 'restore') a.installed = true;
    if (action === 'disable') a.enabled = false;
    if (action === 'enable') a.enabled = true;
  }

  const DESTRUCTIVE: AppAction[] = ['uninstall', 'uninstallFull', 'disable', 'clear'];

  async function runAction(action: AppAction, list?: AppEntry[]) {
    if (!dev) return;
    const sel = list ?? apps.filter((a) => checked.has(a.package));
    if (!sel.length) return;
    if (!supported(action)) {
      pushLog('error', `${$t('apps.act.' + action)}: ${$t('apps.unsupported').replace('{sdk}', String(sdk))}`, 'apps');
      return;
    }
    if (DESTRUCTIVE.includes(action)) {
      const critical = sel.filter((a) => a.critical);
      const systemFull = action === 'uninstallFull' && sel.some((a) => a.system);
      let msg = $t(`apps.confirm.${action}`);
      if (systemFull) msg += '\n\n' + $t('apps.fullSystemWarn');
      if (critical.length) msg += '\n\n' + $t('apps.criticalWarn');
      const ok = await confirmDialog({
        title: $t(`apps.act.${action}`) + ` (${sel.length})`,
        message: msg,
        items: sel.map((a) => (a.critical ? '⚠ ' : '') + nameOf(a)),
        danger: critical.length > 0 || action === 'clear' || action === 'uninstallFull',
        confirmLabel: $t(`apps.act.${action}`)
      });
      if (!ok) return;
    }
    job.onItem = (id, ok, msg) => {
      status.set(id, { ok, msg });
      if (ok) applyLocal(id, action);
    };
    job.onFinished = () => {
      if (!list) checked.clear();
    };
    await job.start((on) => api.appsAction(dev.serial, sel.map((a) => a.package), action, on));
  }

  async function extract(list?: AppEntry[]) {
    if (!dev) return;
    const sel = list ?? apps.filter((a) => checked.has(a.package));
    if (!sel.length) return;
    const dir = await open({ directory: true, multiple: false, title: $t('apps.extractTo') });
    if (typeof dir !== 'string') return;
    job.onItem = (id, ok, msg) => status.set(id, { ok, msg });
    job.onFinished = null;
    await job.start((on) => api.extractApks(dev.serial, sel.map((a) => a.package), dir, on));
  }

  async function install(dirMode: boolean) {
    if (!dev) return;
    const r = dirMode
      ? await open({ directory: true, multiple: true })
      : await open({ multiple: true, filters: [{ name: 'APK', extensions: ['apk'] }] });
    const paths = Array.isArray(r) ? r : typeof r === 'string' ? [r] : [];
    if (!paths.length) return;
    job.onItem = (id, ok, msg) => pushLog(ok ? 'ok' : 'error', `${id}: ${msg}`, 'apps');
    job.onFinished = () => load();
    await job.start((on) =>
      api.installApks(dev.serial, paths, { downgrade: installDowngrade, grantAll: installGrant }, on)
    );
  }

  // --- menu akcji pojedynczej aplikacji (pozycja fixed - nie ucina go przewijana lista) ---
  let menu = $state<{ app: AppEntry; x: number; y: number; up: boolean } | null>(null);

  function openMenu(e: MouseEvent, a: AppEntry) {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const up = r.bottom + 330 > window.innerHeight;
    menu = { app: a, x: Math.max(8, r.right - 260), y: up ? r.top : r.bottom, up };
  }

  type MenuItem = { action: AppAction | 'extract'; icon: typeof Ban; danger?: boolean; show: (a: AppEntry) => boolean };
  const menuItems: MenuItem[] = [
    { action: 'forceStop', icon: Ban, show: (a) => a.installed },
    { action: 'clear', icon: Eraser, danger: true, show: (a) => a.installed },
    { action: 'disable', icon: PowerOff, show: (a) => a.installed && a.enabled },
    { action: 'enable', icon: Power, show: (a) => a.installed && !a.enabled },
    { action: 'uninstall', icon: Trash2, danger: true, show: (a) => a.installed },
    { action: 'uninstallFull', icon: Trash, danger: true, show: (a) => a.installed },
    { action: 'restore', icon: Undo2, show: (a) => !a.installed },
    { action: 'extract', icon: Download, show: (a) => a.installed }
  ];

  function menuRun(item: MenuItem) {
    const a = menu?.app;
    menu = null;
    if (!a) return;
    if (item.action === 'extract') extract([a]);
    else runAction(item.action, [a]);
  }

  const filters: Filter[] = ['user', 'system', 'disabled', 'removed', 'all'];
</script>

<svelte:window onclick={() => (menu = null)} onkeydown={(e) => e.key === 'Escape' && (menu = null)} />

<section class="animate-fade-in space-y-4">
  <div class="flex items-center justify-between gap-3">
    <div class="flex items-center gap-3">
      <h2 class="view-title">{$t('apps.title')}</h2>
      {#if sdk}<span class="badge bg-gold/10 text-gold">Android SDK {sdk}</span>{/if}
    </div>
    <button class="btn" disabled={!ready || loadingList} onclick={load}>
      <RefreshCw size={15} class={loadingList || loadingDetails ? 'animate-spin' : ''} />{$t('common.refresh')}
    </button>
  </div>

  {#if !ready}
    <div class="card text-sm text-warn">{$t('apps.needAdb')}</div>
  {:else}
    <!-- WIDOCZNY postęp ładowania: lista pakietów -> nazwy i ikony -->
    <AppsLoader {stage} done={loadDone} total={loadTotal} current={loadCurrent} failed={loadFailed} onCancel={stopLoad} />

    <div class="card flex items-start gap-2 border-warn/30 bg-warn/5 text-xs text-warn">
      <ShieldAlert size={15} class="mt-px shrink-0" />{$t('apps.debloatInfo')}
    </div>

    <!-- pasek filtrów -->
    <div class="flex flex-wrap items-center gap-2">
      <div class="flex rounded-lg border border-ink-500 bg-ink-800 p-0.5">
        {#each filters as f (f)}
          <button
            class="rounded-md px-3 py-1.5 text-xs transition-colors {filter === f ? 'bg-gold/15 text-gold' : 'text-neutral-400 hover:text-neutral-200'}"
            onclick={() => (filter = f)}
          >
            {$t('apps.f.' + f)} <span class="ml-1 font-mono opacity-70">{counts[f]}</span>
          </button>
        {/each}
      </div>
      <div class="relative min-w-[220px] flex-1">
        <Search size={14} class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-neutral-500" />
        <input class="input !pl-9" placeholder={$t('apps.search')} bind:value={query} />
      </div>
    </div>

    <!-- akcje zbiorcze -->
    <div class="card flex flex-wrap items-center gap-2 !py-2.5">
      <span class="mr-1 text-xs text-neutral-400">{$t('apps.checked')}: <span class="font-mono text-gold">{checked.size}</span></span>
      <button class="btn !py-1.5 text-xs" disabled={!checked.size || job.running} onclick={() => extract()}><Download size={14} />{$t('apps.act.extract')}</button>
      <button class="btn btn-danger !py-1.5 text-xs" disabled={!checked.size || job.running || !supported('uninstall')} onclick={() => runAction('uninstall')}><Trash2 size={14} />{$t('apps.act.uninstall')}</button>
      <button class="btn btn-danger !py-1.5 text-xs" disabled={!checked.size || job.running} onclick={() => runAction('uninstallFull')}><Trash size={14} />{$t('apps.act.uninstallFull')}</button>
      <button class="btn !py-1.5 text-xs" disabled={!checked.size || job.running} onclick={() => runAction('disable')}><PowerOff size={14} />{$t('apps.act.disable')}</button>
      <button class="btn !py-1.5 text-xs" disabled={!checked.size || job.running} onclick={() => runAction('enable')}><Power size={14} />{$t('apps.act.enable')}</button>
      <button class="btn !py-1.5 text-xs" disabled={!checked.size || job.running || !supported('restore')} onclick={() => runAction('restore')}><Undo2 size={14} />{$t('apps.act.restore')}</button>
      <button class="btn !py-1.5 text-xs" disabled={!checked.size || job.running} onclick={() => runAction('forceStop')}><Ban size={14} />{$t('apps.act.forceStop')}</button>
      <button class="btn btn-danger !py-1.5 text-xs" disabled={!checked.size || job.running} onclick={() => runAction('clear')}><Eraser size={14} />{$t('apps.act.clear')}</button>
      <div class="ml-auto flex items-center gap-2">
        <button class="btn btn-gold !py-1.5 text-xs" disabled={job.running} onclick={() => install(false)}><PackagePlus size={14} />{$t('apps.installFiles')}</button>
        <button class="btn !py-1.5 text-xs" disabled={job.running} onclick={() => install(true)}><FolderOpen size={14} />{$t('apps.installFolder')}</button>
      </div>
      <div class="flex w-full justify-end gap-4 text-xs">
        <label class="check"><input type="checkbox" bind:checked={installDowngrade} />{$t('apps.optDowngrade')}</label>
        <label class="check"><input type="checkbox" bind:checked={installGrant} />{$t('apps.optGrant')}</label>
      </div>
    </div>

    <ProgressCard {job} />

    <!-- lista -->
    <div class="card !p-0">
      <div class="flex items-center gap-3 border-b border-ink-600 px-4 py-2 text-xs text-neutral-500">
        <button class="text-neutral-400 hover:text-gold" onclick={toggleAll} title={$t('apps.selectAll')}>
          {#if allVisibleChecked}<CheckSquare size={16} class="text-gold" />{:else}<SquareIcon size={16} />{/if}
        </button>
        <span>{visible.length} {$t('apps.shown')}</span>
        <span class="ml-auto hidden lg:inline">{$t('apps.menuHint')}</span>
      </div>
      <div class="max-h-[58vh] overflow-y-auto">
        {#if loadingList}
          {#each Array(6) as _, i (i)}
            <div class="flex items-center gap-3 border-b border-ink-700 px-4 py-2.5">
              <div class="h-4 w-4 rounded bg-ink-600"></div>
              <div class="h-[38px] w-[38px] animate-pulse rounded-xl bg-ink-600"></div>
              <div class="flex-1 space-y-1.5">
                <div class="h-3 w-40 animate-pulse rounded bg-ink-600"></div>
                <div class="h-2.5 w-56 animate-pulse rounded bg-ink-700"></div>
              </div>
            </div>
          {/each}
        {:else}
          {#each visible as a (a.package)}
            {@const d = details.get(a.package)}
            {@const st = status.get(a.package)}
            <div class="flex items-center gap-3 border-b border-ink-700 px-4 py-2 last:border-0 hover:bg-ink-700/60 {checked.has(a.package) ? 'bg-gold/5' : ''}">
              <input
                type="checkbox"
                class="accent-[#c9a249]"
                checked={checked.has(a.package)}
                onchange={() => (checked.has(a.package) ? checked.delete(a.package) : checked.add(a.package))}
              />
              <AppIcon pkg={a.package} details={d ?? null} loading={loadingDetails && !d} size={38} />
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <span class="truncate text-sm font-medium {a.installed ? 'text-neutral-100' : 'text-neutral-500 line-through'}">{d?.label ?? a.package}</span>
                  {#if d?.versionName}<span class="shrink-0 font-mono text-[11px] text-neutral-500">v{d.versionName}</span>{/if}
                </div>
                <div class="truncate font-mono text-xs text-neutral-500">{a.package}</div>
              </div>
              <div class="flex shrink-0 items-center gap-1.5">
                {#if d?.apkSize}<span class="hidden font-mono text-[11px] text-neutral-600 md:inline">{humanBytes(d.apkSize)}</span>{/if}
                {#if a.critical}<span class="badge bg-err/15 text-err" title={$t('apps.criticalTip')}>{$t('apps.b.critical')}</span>{/if}
                {#if a.system}<span class="badge bg-sky-500/10 text-sky-300">{$t('apps.b.system')}</span>{:else}<span class="badge bg-ok/10 text-ok">{$t('apps.b.user')}</span>{/if}
                {#if !a.enabled}<span class="badge bg-warn/15 text-warn">{$t('apps.b.disabled')}</span>{/if}
                {#if !a.installed}<span class="badge bg-neutral-500/20 text-neutral-400">{$t('apps.b.removed')}</span>{/if}
                {#if st}
                  <span title={st.msg}>{#if st.ok}<CircleCheck size={16} class="text-ok" />{:else}<CircleX size={16} class="text-err" />{/if}</span>
                {/if}
                <button
                  class="icon-btn ml-1 {menu?.app.package === a.package ? 'bg-ink-600 text-gold' : ''}"
                  title={$t('apps.actions')}
                  disabled={job.running}
                  onclick={(e) => openMenu(e, a)}
                >
                  <EllipsisVertical size={16} />
                </button>
              </div>
            </div>
          {:else}
            <div class="p-6 text-center text-sm text-neutral-600">{$t('apps.none')}</div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</section>

{#if menu}
  {@const m = menu}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="fixed z-40 w-[260px] animate-fade-in overflow-hidden rounded-xl border border-ink-400 bg-ink-800 py-1 shadow-2xl"
    style="left:{m.x}px;{m.up ? `bottom:${window.innerHeight - m.y + 4}px` : `top:${m.y + 4}px`}"
    onclick={(e) => e.stopPropagation()}
  >
    <div class="border-b border-ink-600 px-3 py-2">
      <div class="truncate text-sm font-medium text-neutral-100">{details.get(m.app.package)?.label ?? m.app.package}</div>
      <div class="truncate font-mono text-[11px] text-neutral-500">{m.app.package}</div>
    </div>
    {#each menuItems.filter((i) => i.show(m.app)) as item (item.action)}
      {@const ok = item.action === 'extract' || supported(item.action)}
      <button
        class="flex w-full items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors disabled:opacity-35 {item.danger ? 'text-neutral-200 hover:bg-err/10 hover:text-err' : 'text-neutral-200 hover:bg-ink-600 hover:text-gold'}"
        disabled={!ok}
        title={ok ? $t('apps.cmd.' + item.action) : $t('apps.unsupported').replace('{sdk}', String(sdk))}
        onclick={() => menuRun(item)}
      >
        <item.icon size={15} class="shrink-0" />
        <span class="flex-1">{$t('apps.act.' + item.action)}</span>
      </button>
    {/each}
    {#if m.app.critical}
      <div class="border-t border-ink-600 px-3 py-2 text-[11px] text-err">⚠ {$t('apps.criticalTip')}</div>
    {/if}
  </div>
{/if}
