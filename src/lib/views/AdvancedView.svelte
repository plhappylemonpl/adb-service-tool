<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { TerminalSquare, Play, FileArchive, Upload, Sparkles, RefreshCw, LoaderCircle } from 'lucide-svelte';
  import { updater } from '$lib/updater.svelte';
  import { toast } from '$lib/components/Toast.svelte';
  import { api } from '$lib/api';
  import { selectedDevice, Job, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import ProgressCard from '$lib/components/ProgressCard.svelte';

  const job = new Job('sideload');
  const dev = $derived(selectedDevice());

  let cmd = $state('getprop ro.product.model');
  let running = $state(false);
  let history = $state<{ cmd: string; out: string; err: boolean }[]>([]);
  let zip = $state('');

  async function run() {
    if (!dev || !cmd.trim()) return;
    running = true;
    const c = cmd.trim();
    try {
      const r = await api.shellCommand(dev.serial, c);
      history.push({ cmd: c, out: (r.stdout + (r.stderr ? '\n' + r.stderr : '')).trimEnd(), err: r.status !== 0 });
    } catch (e) {
      history.push({ cmd: c, out: errText(e), err: true });
    } finally {
      running = false;
      if (history.length > 50) history.splice(0, history.length - 50);
    }
  }

  async function pickZip() {
    const f = await open({ multiple: false, filters: [{ name: 'OTA / ZIP', extensions: ['zip'] }] });
    if (typeof f === 'string') zip = f;
  }

  async function checkUpdates() {
    await updater.check(true);
    if (updater.error) toast('error', $t('update.checkFailed'), updater.error);
    else if (updater.info && !updater.info.available) toast('ok', $t('update.noneFound'), `v${updater.info.current}`);
  }

  function unskip() {
    try {
      localStorage.removeItem('adbst.update.skip');
    } catch {
      /* brak storage */
    }
    updater.skipped = null;
  }

  async function sideload() {
    if (!dev || !zip) return;
    pushLog('info', `sideload: ${zip}`, 'sideload');
    await job.start((on) => api.startSideload(dev.serial, zip, on));
  }
</script>

<section class="animate-fade-in space-y-4">
  <h2 class="view-title">{$t('adv.title')}</h2>

  <div class="card space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-gold"><Sparkles size={16} />{$t('update.section')}</div>
    <div class="flex flex-wrap items-center gap-3 text-sm">
      <button class="btn btn-gold !py-1.5" disabled={updater.checking || updater.job.running} onclick={checkUpdates}>
        {#if updater.checking}<LoaderCircle size={15} class="animate-spin" />{$t('update.checking')}{:else}<RefreshCw size={15} />{$t('update.check')}{/if}
      </button>
      <label class="toggle text-sm">
        <input type="checkbox" checked={updater.autoCheck} onchange={(e) => updater.setAuto(e.currentTarget.checked)} /><span></span>{$t('update.auto')}
      </label>
    </div>
    {#if updater.info}
      <div class="flex flex-wrap gap-x-5 gap-y-1 text-xs text-neutral-400">
        <span>{$t('update.current')}: <span class="font-mono text-neutral-200">{updater.info.current}</span></span>
        {#if updater.info.latest}<span>{$t('update.latest')}: <span class="font-mono text-gold">{updater.info.latest}</span></span>{/if}
        <span>{$t('update.kind')}: <span class="text-neutral-200">{$t('update.kind.' + updater.info.kind)}</span></span>
        {#if updater.info.available}
          <button class="text-gold underline" onclick={() => (updater.open = true)}>{$t('update.title')}</button>
        {/if}
      </div>
    {/if}
    {#if updater.skipped}
      <div class="flex items-center gap-2 text-xs text-neutral-500">
        {$t('update.skippedInfo').replace('{v}', updater.skipped)}
        <button class="underline" onclick={unskip}>{$t('update.unskip')}</button>
      </div>
    {/if}
  </div>

  <div class="card space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-gold"><TerminalSquare size={16} />{$t('adv.shell')}</div>
    <form class="flex gap-2" onsubmit={(e) => { e.preventDefault(); run(); }}>
      <span class="self-center font-mono text-sm text-gold">$</span>
      <input class="input flex-1 font-mono" bind:value={cmd} disabled={!dev || dev.mode !== 'adb' && dev.mode !== 'recovery'} />
      <button class="btn btn-gold" disabled={running || !dev}><Play size={15} />{$t('adv.run')}</button>
    </form>
    <div class="max-h-[40vh] space-y-3 overflow-y-auto rounded-lg border border-ink-600 bg-black p-3 font-mono text-xs">
      {#each history as h, i (i)}
        <div>
          <div class="text-gold">$ {h.cmd}</div>
          <pre class="select-text whitespace-pre-wrap break-all {h.err ? 'text-err' : 'text-neutral-300'}">{h.out || '—'}</pre>
        </div>
      {:else}
        <div class="text-neutral-600">{$t('adv.shellHint')}</div>
      {/each}
    </div>
  </div>

  <div class="card space-y-3">
    <div class="flex items-center gap-2 text-sm font-medium text-gold"><FileArchive size={16} />{$t('adv.sideload')}</div>
    <p class="text-xs text-neutral-500">{$t('adv.sideloadHint')}</p>
    <div class="flex gap-2">
      <input class="input flex-1" bind:value={zip} placeholder="update.zip" />
      <button class="btn" onclick={pickZip}>{$t('adv.pickZip')}</button>
      <button class="btn btn-gold" disabled={!dev || dev.mode !== 'sideload' || !zip || job.running} onclick={sideload}>
        <Upload size={15} />{$t('adv.sideloadStart')}
      </button>
    </div>
    <ProgressCard {job} />
  </div>
</section>
