<script lang="ts">
  import { onMount } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { open as openUrl } from '@tauri-apps/plugin-shell';
  import {
    RefreshCw, CircleCheck, CircleX, CircleHelp, Download, FolderOpen, ExternalLink, ShieldCheck,
    Usb, TriangleAlert, KeyRound, Save, Terminal
  } from 'lucide-svelte';
  import { api, type DriversReport, type DriverStatus } from '$lib/api';
  import { Job, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import ProgressCard from '$lib/components/ProgressCard.svelte';

  const job = new Job('drivers');
  let report = $state<DriversReport | null>(null);
  let loading = $state(false);
  let editing = $state<Record<string, { url: string; args: string; sha256: string }>>({});

  // Linux: udev
  let udevBusy = $state(false);
  let askPassword = $state(false);
  let passwordReason = $state('');
  let password = $state('');
  let wrongPassword = $state(false);

  const isWindows = $derived(report?.os === 'windows');
  const isLinux = $derived(report?.os === 'linux');

  async function refresh() {
    loading = true;
    try {
      report = await api.driversReport();
      for (const d of report.drivers) {
        editing[d.id] ??= { url: d.url ?? '', args: d.args, sha256: d.sha256 ?? '' };
      }
    } catch (e) {
      pushLog('error', errText(e), 'drivers');
    } finally {
      loading = false;
    }
  }

  onMount(refresh);

  async function saveOverride(d: DriverStatus) {
    const e = editing[d.id];
    try {
      await api.driversSetOverride(d.id, e.url || null, e.args || null, e.sha256 || null);
      pushLog('ok', `${d.name}: ${$t('drivers.saved')}`, 'drivers');
      await refresh();
    } catch (err) {
      pushLog('error', errText(err), 'drivers');
    }
  }

  async function install(d: DriverStatus, local: 'file' | 'dir' | null) {
    let path: string | null = null;
    if (local === 'file') {
      const r = await open({ multiple: false, filters: [{ name: 'Driver', extensions: ['exe', 'msi', 'zip', 'inf'] }] });
      if (typeof r !== 'string') return;
      path = r;
    } else if (local === 'dir') {
      const r = await open({ directory: true, multiple: false });
      if (typeof r !== 'string') return;
      path = r;
    }
    job.onFinished = () => refresh();
    await job.start((on) => api.driverInstall(d.id, path, on));
  }

  async function installUdev(withPassword: boolean) {
    udevBusy = true;
    wrongPassword = false;
    try {
      const r = await api.udevInstall(withPassword ? password : null, (e) => job.handle(e));
      password = '';
      switch (r.result) {
        case 'installed':
          askPassword = false;
          pushLog('ok', `${$t('drivers.udevDone')}: ${r.path} (${r.source})`, 'udev');
          await refresh();
          break;
        case 'needPassword':
          passwordReason = r.reason;
          askPassword = true;
          break;
        case 'wrongPassword':
          wrongPassword = true;
          askPassword = true;
          break;
        case 'cancelled':
          pushLog('warn', $t('drivers.udevCancelled'), 'udev');
          break;
      }
    } catch (e) {
      password = '';
      pushLog('error', errText(e), 'udev');
    } finally {
      udevBusy = false;
    }
  }

  function kindLabel(k: string): string {
    return $t('usbkind.' + k, k);
  }
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex items-center justify-between">
    <div>
      <h2 class="view-title">{$t('drivers.title')}</h2>
      <p class="text-xs text-neutral-500">{isWindows ? $t('drivers.subWin') : isLinux ? $t('drivers.subLinux') : ''}</p>
    </div>
    <button class="btn" onclick={refresh} disabled={loading}><RefreshCw size={15} class={loading ? 'animate-spin' : ''} />{$t('common.refresh')}</button>
  </div>

  {#if report?.problems.length}
    <div class="card space-y-1 border-warn/40 bg-warn/5 text-xs text-warn">
      {#each report.problems as p (p)}<div class="flex items-start gap-2"><TriangleAlert size={13} class="mt-px shrink-0" />{p}</div>{/each}
    </div>
  {:else if report}
    <div class="card flex items-center gap-2 border-ok/30 text-sm text-ok"><ShieldCheck size={16} />{$t('drivers.allGood')}</div>
  {/if}

  <ProgressCard {job} />

  <!-- ============ WINDOWS ============ -->
  {#if isWindows && report}
    <div class="grid gap-3 xl:grid-cols-2">
      {#each report.drivers as d (d.id)}
        <div class="card space-y-3">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="font-medium text-neutral-100">{d.name}</div>
              <div class="text-xs text-neutral-500">{d.description}</div>
            </div>
            {#if d.installed === true}
              <span class="badge flex shrink-0 items-center gap-1 bg-ok/15 text-ok"><CircleCheck size={12} />{$t('drivers.installed')}</span>
            {:else if d.installed === false}
              <span class="badge flex shrink-0 items-center gap-1 bg-err/15 text-err"><CircleX size={12} />{$t('drivers.missing')}</span>
            {:else}
              <span class="badge flex shrink-0 items-center gap-1 bg-neutral-500/20 text-neutral-400"><CircleHelp size={12} />?</span>
            {/if}
          </div>
          {#if d.foundIn.length}<div class="font-mono text-[11px] text-neutral-500">INF: {d.foundIn.join(', ')}</div>{/if}
          {#if d.devicesMissingDriver > 0}
            <div class="text-xs text-err">{d.devicesMissingDriver} {$t('drivers.devNoDriver')}</div>
          {/if}

          {#if d.kind !== 'builtin'}
            <div class="space-y-2">
              <input class="input font-mono text-xs" placeholder={$t('drivers.urlPh')} bind:value={editing[d.id].url} />
              <div class="grid grid-cols-2 gap-2">
                <input class="input font-mono text-xs" placeholder={$t('drivers.argsPh')} bind:value={editing[d.id].args} />
                <input class="input font-mono text-xs" placeholder="SHA-256 (opc.)" bind:value={editing[d.id].sha256} />
              </div>
            </div>
            <div class="flex flex-wrap gap-2">
              <button class="btn btn-gold !py-1.5 text-xs" disabled={job.running || !d.url} onclick={() => install(d, null)}>
                <Download size={14} />{$t('drivers.autoInstall')}
              </button>
              <button class="btn !py-1.5 text-xs" disabled={job.running} onclick={() => install(d, 'file')}><FolderOpen size={14} />{$t('drivers.fromFile')}</button>
              <button class="btn !py-1.5 text-xs" disabled={job.running} onclick={() => install(d, 'dir')}><FolderOpen size={14} />{$t('drivers.fromInfDir')}</button>
              <button class="btn !py-1.5 text-xs" onclick={() => saveOverride(d)}><Save size={14} />{$t('drivers.save')}</button>
              {#if d.officialPage}
                <button class="btn !py-1.5 text-xs" onclick={() => openUrl(d.officialPage!)}><ExternalLink size={14} />{$t('drivers.page')}</button>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
    <p class="text-[11px] text-neutral-600">{$t('drivers.uacHint')}</p>
  {/if}

  <!-- ============ LINUX ============ -->
  {#if isLinux && report?.udev}
    {@const u = report.udev}
    <div class="card space-y-3">
      <div class="flex items-center justify-between gap-3">
        <div class="text-sm font-medium text-gold">{$t('drivers.udevTitle')}</div>
        <button class="btn btn-gold" disabled={udevBusy} onclick={() => installUdev(false)}>
          <ShieldCheck size={15} class={udevBusy ? 'animate-pulse' : ''} />{$t('drivers.udevInstall')}
        </button>
      </div>
      <div class="grid gap-3 text-xs md:grid-cols-2">
        <div>
          <div class="mb-1 text-neutral-500">{$t('drivers.covered')}</div>
          <div class="flex flex-wrap gap-1">
            {#each u.covered as c (c)}<span class="badge bg-ok/10 text-ok">{c}</span>{/each}
            {#if !u.covered.length}<span class="text-neutral-600">—</span>{/if}
          </div>
        </div>
        <div>
          <div class="mb-1 text-neutral-500">{$t('drivers.notCovered')}</div>
          <div class="flex flex-wrap gap-1">
            {#each u.missing as c (c)}<span class="badge bg-err/15 text-err">{c}</span>{/each}
            {#if !u.missing.length}<span class="text-ok">—</span>{/if}
          </div>
        </div>
      </div>
      <div class="space-y-0.5 font-mono text-[11px] text-neutral-500">
        {#each u.rulesFiles as f (f)}<div>{f}</div>{/each}
      </div>
      <div class="flex flex-wrap gap-x-6 gap-y-1 text-xs text-neutral-400">
        <span>pkexec: <b class={u.pkexec ? 'text-ok' : 'text-err'}>{u.pkexec ? 'OK' : '—'}</b></span>
        <span>sudo: <b class={u.sudo ? 'text-ok' : 'text-err'}>{u.sudo ? 'OK' : '—'}</b></span>
        <span>{$t('drivers.group')} adbusers ({u.user}): <b class={u.inAdbusers ? 'text-ok' : 'text-warn'}>{u.inAdbusers ? $t('common.yes') : $t('common.no')}</b></span>
      </div>
      {#if u.packageHint}
        <div class="flex items-center gap-2 rounded-md border border-ink-500 bg-black px-3 py-2 font-mono text-[12px] text-neutral-300">
          <Terminal size={13} class="shrink-0 text-gold" />{$t('drivers.altPkg')}: <span class="select-text text-gold">{u.packageHint}</span>
        </div>
      {/if}
      <p class="text-[11px] text-neutral-600">{$t('drivers.udevHint')}</p>
    </div>
  {/if}

  <!-- ============ urządzenia USB (oba systemy) ============ -->
  {#if report}
    <div class="card !p-0">
      <div class="flex items-center gap-2 border-b border-ink-600 px-4 py-2.5 text-sm text-neutral-300"><Usb size={15} class="text-gold" />{$t('drivers.usbDevices')} ({report.devices.length})</div>
      {#each report.devices as d, i (i)}
        <div class="flex items-center gap-3 border-b border-ink-700 px-4 py-2 text-xs last:border-0">
          <span class="w-24 shrink-0 font-mono text-neutral-500">{d.vid}:{d.pid}</span>
          <div class="min-w-0 flex-1">
            <div class="truncate text-sm text-neutral-200">{d.vendorName ?? d.manufacturer ?? '?'} {d.product ?? ''}</div>
            <div class="truncate font-mono text-neutral-500">{d.serial ?? ''} {d.interfaces.join(' ')}</div>
          </div>
          <span class="badge bg-gold/10 text-gold">{kindLabel(d.kind)}</span>
          {#if isWindows}
            <span class="badge {d.hasDriver ? 'bg-ok/10 text-ok' : 'bg-err/15 text-err'}">{d.hasDriver ? d.drivers.join(', ') : $t('drivers.noDriver')}</span>
          {:else}
            {#if d.drivers.length}<span class="badge bg-sky-500/10 text-sky-300">{d.drivers.join(', ')}</span>{/if}
            {#if d.accessible !== null}
              <span class="badge {d.accessible ? 'bg-ok/10 text-ok' : 'bg-err/15 text-err'}">{d.accessible ? $t('drivers.access') : $t('drivers.noAccess')}</span>
            {/if}
          {/if}
        </div>
      {:else}
        <div class="p-5 text-center text-sm text-neutral-600">{$t('drivers.noUsb')}</div>
      {/each}
    </div>
  {/if}
</section>

{#if askPassword}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6 backdrop-blur-sm">
    <form class="card w-full max-w-md animate-fade-in space-y-4" onsubmit={(e) => { e.preventDefault(); installUdev(true); }}>
      <div class="flex items-center gap-2 font-semibold text-gold"><KeyRound size={18} />{$t('drivers.pwTitle')}</div>
      <p class="text-sm text-neutral-400">{$t('drivers.pwText')}</p>
      {#if passwordReason}<p class="font-mono text-[11px] text-neutral-600">{passwordReason}</p>{/if}
      <input type="password" class="input" autocomplete="current-password" placeholder="sudo" bind:value={password} />
      {#if wrongPassword}<p class="text-xs text-err">{$t('drivers.pwWrong')}</p>{/if}
      <div class="flex justify-end gap-2">
        <button type="button" class="btn" onclick={() => { askPassword = false; password = ''; }}>{$t('common.cancel')}</button>
        <button type="submit" class="btn btn-gold" disabled={!password || udevBusy}>{$t('drivers.pwOk')}</button>
      </div>
    </form>
  </div>
{/if}
