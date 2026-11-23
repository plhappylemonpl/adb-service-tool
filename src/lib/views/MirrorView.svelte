<script lang="ts">
  import { onMount } from 'svelte';
  import {
    MonitorSmartphone, Play, Square, Camera, FolderOpen, Download, RefreshCw, Copy, CircleCheck, TriangleAlert, Terminal
  } from 'lucide-svelte';
  import { api, type ScrcpyOptions, type ScrcpyStatus, type Screenshot } from '$lib/api';
  import { selectedDevice, Job, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText, humanBytes, basename } from '$lib/format';
  import ProgressCard from '$lib/components/ProgressCard.svelte';

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && dev.mode === 'adb');

  let status = $state<ScrcpyStatus | null>(null);
  let checking = $state(false);
  const dlJob = new Job('scrcpy');
  const session = new Job('scrcpy');

  // presety jakości - dla słabych kabli / starych urządzeń
  const presets = [
    { id: 'max', maxSize: 0, bitRateMbps: 16, maxFps: 60 },
    { id: 'balanced', maxSize: 1280, bitRateMbps: 8, maxFps: 60 },
    { id: 'low', maxSize: 1024, bitRateMbps: 4, maxFps: 30 },
    { id: 'potato', maxSize: 800, bitRateMbps: 2, maxFps: 24 }
  ] as const;

  let opts = $state<ScrcpyOptions>({
    maxSize: 1280,
    bitRateMbps: 8,
    maxFps: 60,
    stayAwake: true,
    showTouches: false,
    turnScreenOff: false,
    noAudio: true,
    alwaysOnTop: false,
    borderless: false,
    fullscreen: false,
    viewOnly: false
  });

  const sizeSteps = [0, 2560, 1920, 1600, 1280, 1024, 800, 640];
  let sizeIdx = $state(4);
  $effect(() => {
    opts.maxSize = sizeSteps[sizeIdx];
  });

  function applyPreset(p: (typeof presets)[number]) {
    sizeIdx = Math.max(0, sizeSteps.indexOf(p.maxSize));
    opts.bitRateMbps = p.bitRateMbps;
    opts.maxFps = p.maxFps;
  }

  async function refreshStatus() {
    checking = true;
    try {
      status = await api.scrcpyStatus();
    } catch (e) {
      pushLog('error', errText(e), 'scrcpy');
    } finally {
      checking = false;
    }
  }

  onMount(refreshStatus);

  async function download() {
    dlJob.onFinished = () => refreshStatus();
    await dlJob.start((on) => api.scrcpyDownload(on));
  }

  async function startMirror() {
    if (!dev) return;
    const title = `${dev.modelHint ?? dev.serial} — ADB Service Tool`;
    await session.start((on) => api.scrcpyStart(dev.serial, title, $state.snapshot(opts), on));
  }

  async function copyCmd() {
    if (!status?.distro) return;
    try {
      await navigator.clipboard.writeText(status.distro.command);
      pushLog('ok', $t('common.copied'));
    } catch {
      /* brak schowka */
    }
  }

  // --- zrzuty ekranu ---
  let shots = $state<Screenshot[]>([]);
  let current = $state<Screenshot | null>(null);
  let shooting = $state(false);

  async function screenshot() {
    if (!dev) return;
    shooting = true;
    try {
      const s = await api.takeScreenshot(dev.serial);
      shots.unshift(s);
      if (shots.length > 12) shots.pop();
      current = s;
      pushLog('ok', `${$t('mirror.shotSaved')}: ${s.path}`, 'screenshot');
    } catch (e) {
      pushLog('error', errText(e), 'screenshot');
    } finally {
      shooting = false;
    }
  }

  async function reveal(p: string) {
    try {
      await api.revealPath(p);
    } catch (e) {
      pushLog('error', errText(e));
    }
  }
</script>

<section class="animate-fade-in space-y-4">
  <h2 class="view-title">{$t('mirror.title')}</h2>

  {#if !ready}
    <div class="card text-sm text-warn">{$t('mirror.needAdb')}</div>
  {/if}

  <div class="grid gap-4 2xl:grid-cols-2">
    <!-- ================= scrcpy ================= -->
    <div class="space-y-4">
      <div class="card space-y-3">
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-2 text-sm font-medium text-gold"><MonitorSmartphone size={16} />{$t('mirror.scrcpy')}</div>
          <button class="icon-btn" title={$t('common.refresh')} onclick={refreshStatus}><RefreshCw size={14} class={checking ? 'animate-spin' : ''} /></button>
        </div>

        {#if status?.path}
          <div class="flex items-center gap-2 text-xs">
            <CircleCheck size={14} class="text-ok" />
            <span class="text-neutral-300">scrcpy {status.version ?? ''}</span>
            <span class="truncate font-mono text-neutral-500" title={status.path}>{status.path}</span>
            {#if status.bundled}<span class="badge bg-gold/10 text-gold">{$t('mirror.bundled')}</span>{/if}
          </div>
        {:else if status}
          <div class="space-y-2 rounded-lg border border-warn/30 bg-warn/5 p-3 text-xs">
            <div class="flex items-center gap-2 text-warn"><TriangleAlert size={14} />{$t('mirror.missing')}</div>
            {#if status.canDownload}
              <p class="text-neutral-400">{$t('mirror.downloadHint')}</p>
              <button class="btn btn-gold !py-1.5 text-xs" disabled={dlJob.running} onclick={download}>
                <Download size={14} />{$t('mirror.download')}
              </button>
            {:else if status.distro}
              <p class="text-neutral-400">{$t('mirror.installHint')} <span class="text-neutral-200">{status.distro.name}</span>:</p>
              <div class="flex items-center gap-2 rounded-md border border-ink-500 bg-black px-3 py-2 font-mono text-[12px] text-gold">
                <Terminal size={13} class="shrink-0" />
                <span class="flex-1 select-text break-all">{status.distro.command}</span>
                <button class="icon-btn" title={$t('common.copy')} onclick={copyCmd}><Copy size={13} /></button>
              </div>
              {#if status.distro.note}<p class="text-neutral-500">{status.distro.note}</p>{/if}
              <p class="text-neutral-500">{$t('mirror.afterInstall')}</p>
            {:else}
              <p class="text-neutral-400">{$t('mirror.manual')}</p>
            {/if}
          </div>
        {/if}
        <ProgressCard job={dlJob} />
      </div>

      <div class="card space-y-4">
        <div class="text-sm font-medium text-neutral-300">{$t('mirror.options')}</div>

        <div class="flex flex-wrap gap-1.5">
          {#each presets as p (p.id)}
            <button
              class="chip {opts.maxSize === p.maxSize && opts.bitRateMbps === p.bitRateMbps && opts.maxFps === p.maxFps ? 'chip-on' : ''}"
              onclick={() => applyPreset(p)}
            >{$t('mirror.preset.' + p.id)}</button>
          {/each}
        </div>

        <label class="block">
          <div class="mb-1 flex justify-between text-xs">
            <span class="text-neutral-500">{$t('mirror.maxSize')}</span>
            <span class="font-mono text-gold">{opts.maxSize === 0 ? $t('mirror.native') : `${opts.maxSize} px`}</span>
          </div>
          <input type="range" min="0" max={sizeSteps.length - 1} step="1" bind:value={sizeIdx} class="slider" />
        </label>
        <label class="block">
          <div class="mb-1 flex justify-between text-xs">
            <span class="text-neutral-500">{$t('mirror.bitrate')}</span>
            <span class="font-mono text-gold">{opts.bitRateMbps} Mb/s</span>
          </div>
          <input type="range" min="1" max="32" step="1" bind:value={opts.bitRateMbps} class="slider" />
        </label>
        <label class="block">
          <div class="mb-1 flex justify-between text-xs">
            <span class="text-neutral-500">{$t('mirror.fps')}</span>
            <span class="font-mono text-gold">{opts.maxFps === 0 ? '∞' : opts.maxFps}</span>
          </div>
          <input type="range" min="0" max="120" step="5" bind:value={opts.maxFps} class="slider" />
        </label>

        <div class="grid gap-2 sm:grid-cols-2">
          <label class="toggle"><input type="checkbox" bind:checked={opts.stayAwake} /><span></span>{$t('mirror.stayAwake')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.showTouches} /><span></span>{$t('mirror.showTouches')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.turnScreenOff} /><span></span>{$t('mirror.screenOff')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.noAudio} /><span></span>{$t('mirror.noAudio')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.alwaysOnTop} /><span></span>{$t('mirror.onTop')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.borderless} /><span></span>{$t('mirror.borderless')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.fullscreen} /><span></span>{$t('mirror.fullscreen')}</label>
          <label class="toggle"><input type="checkbox" bind:checked={opts.viewOnly} /><span></span>{$t('mirror.viewOnly')}</label>
        </div>

        {#if !session.running}
          <button class="btn btn-gold w-full justify-center py-3 text-base" disabled={!ready || !status?.path} onclick={startMirror}>
            <Play size={17} />{$t('mirror.start')}
          </button>
        {:else}
          <div class="flex items-center gap-3">
            <span class="flex flex-1 items-center gap-2 text-sm text-ok"><span class="h-2 w-2 animate-pulse rounded-full bg-ok"></span>{$t('mirror.running')}</span>
            <button class="btn btn-danger" onclick={() => session.cancel()}><Square size={15} />{$t('mirror.stop')}</button>
          </div>
        {/if}
        <p class="text-[11px] text-neutral-600">{$t('mirror.controlsHint')}</p>
      </div>
    </div>

    <!-- ================= zrzuty ekranu ================= -->
    <div class="card flex flex-col space-y-3">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2 text-sm font-medium text-gold"><Camera size={16} />{$t('mirror.screenshots')}</div>
        <button class="btn btn-gold" disabled={!ready || shooting} onclick={screenshot}>
          <Camera size={15} class={shooting ? 'animate-pulse' : ''} />{$t('mirror.takeShot')}
        </button>
      </div>

      <div class="flex min-h-[340px] flex-1 items-center justify-center rounded-lg border border-ink-600 bg-black p-3">
        {#if current}
          <img src={current.dataUrl} alt="screenshot" class="max-h-[60vh] max-w-full animate-fade-in rounded-md object-contain shadow-2xl" />
        {:else if shooting}
          <div class="animate-pulse text-sm text-neutral-500">{$t('mirror.capturing')}</div>
        {:else}
          <div class="text-center text-sm text-neutral-600">{$t('mirror.noShot')}</div>
        {/if}
      </div>

      {#if current}
        <div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-neutral-400">
          <span class="font-mono">{current.width}×{current.height}</span>
          <span class="font-mono">{humanBytes(current.bytes)}</span>
          <span class="flex-1 truncate font-mono text-neutral-500" title={current.path}>{current.path}</span>
          <button class="btn !py-1 text-xs" onclick={() => current && reveal(current.path)}><FolderOpen size={13} />{$t('mirror.reveal')}</button>
        </div>
      {/if}

      {#if shots.length > 1}
        <div class="flex gap-2 overflow-x-auto pb-1">
          {#each shots as s (s.path)}
            <button
              class="shrink-0 overflow-hidden rounded border-2 {current?.path === s.path ? 'border-gold' : 'border-transparent opacity-70 hover:opacity-100'}"
              title={basename(s.path)}
              onclick={() => (current = s)}
            >
              <img src={s.dataUrl} alt={s.takenAt} class="h-20 w-auto" />
            </button>
          {/each}
        </div>
      {/if}
    </div>
  </div>

</section>
