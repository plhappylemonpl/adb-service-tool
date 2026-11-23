<script lang="ts">
  import { save } from '@tauri-apps/plugin-dialog';
  import { Play, Square, Pause, Trash2, Download, Search, Eraser, ArrowDownToLine } from 'lucide-svelte';
  import { api, type LogLine, type LogcatEvent } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && (dev.mode === 'adb' || dev.mode === 'recovery'));

  const KEEP = 4000; // linie trzymane w GUI
  const RENDER = 700; // linie renderowane (reszta: eksport do pliku)

  // $state.raw: tablica podmieniana w całości - bez głębokich proxy (wydajność)
  let lines = $state.raw<LogLine[]>([]);
  let jobId = $state<string | null>(null);
  let running = $state(false);
  let paused = $state(false);
  let follow = $state(true);
  let stats = $state({ total: 0, matched: 0, perSec: 0, buffered: 0 });
  let dropped = $state(0);
  let ended = $state<string | null>(null);

  let pattern = $state('');
  let minLevel = $state('V');
  let ci = $state(true);
  let preset = $state('all');
  let regexError = $state<string | null>(null);
  let bufKernel = $state(false);
  let tail = $state(500);

  let pending = $state.raw<LogLine[]>([]);
  let box: HTMLDivElement | undefined = $state();

  const presets: { id: string; pattern: string; level: string }[] = [
    { id: 'all', pattern: '', level: 'V' },
    { id: 'errors', pattern: '', level: 'E' },
    { id: 'crash', pattern: 'FATAL EXCEPTION|AndroidRuntime|ANR in|tombstone|Fatal signal|SIGSEGV|SIGABRT|backtrace|DEBUG', level: 'V' },
    { id: 'kernel', pattern: 'kernel panic|panic|oops|Unable to handle kernel|watchdog|bite|subsys|SSR|modem.*crash|restart_level|ramdump|lowmemorykiller|thermal.*shutdown', level: 'V' },
    { id: 'battery', pattern: 'batter|healthd|charg|power_supply|BatteryService|fuel|bms|smb|usbpd|vbus|overheat|thermal|cycle', level: 'V' },
    { id: 'display', pattern: 'touch|tsp|SurfaceFlinger|display|panel|mdss|sde_|hwcomposer|backlight', level: 'V' },
    { id: 'radio', pattern: 'RIL|radio|modem|SIM|imei|baseband|qmi|IMS|telephony', level: 'V' }
  ];

  function onEvent(e: LogcatEvent) {
    switch (e.kind) {
      case 'lines':
        dropped += e.dropped;
        if (paused) {
          pending = pending.concat(e.lines).slice(-KEEP);
        } else {
          const n = lines.concat(e.lines);
          lines = n.length > KEEP ? n.slice(n.length - KEEP) : n;
        }
        break;
      case 'reset':
        pending = [];
        dropped = 0;
        lines = e.lines.slice(-KEEP);
        break;
      case 'stats':
        stats = e;
        break;
      case 'ended':
        ended = e.message;
        running = false;
        jobId = null;
        break;
    }
  }

  async function start() {
    if (!dev || running) return;
    lines = [];
    pending = [];
    dropped = 0;
    ended = null;
    running = true;
    try {
      const buffers = ['main', 'system', 'crash', ...(bufKernel ? ['kernel'] : [])];
      jobId = await api.logcatStart(dev.serial, { buffers, tail }, onEvent);
      await applyFilter();
    } catch (e) {
      running = false;
      pushLog('error', errText(e), 'logcat');
    }
  }

  async function stop() {
    if (jobId) await api.cancelJob(jobId).catch(() => {});
  }

  async function applyFilter() {
    if (!jobId) return;
    try {
      await api.logcatFilter(jobId, pattern, minLevel, ci);
      regexError = null;
    } catch (e) {
      regexError = errText(e);
    }
  }

  // zmiana filtra -> po 300 ms do Rusta (bez restartu logcat)
  let debounce: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    void pattern;
    void minLevel;
    void ci;
    clearTimeout(debounce);
    debounce = setTimeout(applyFilter, 300);
    return () => clearTimeout(debounce);
  });

  function usePreset(p: (typeof presets)[number]) {
    preset = p.id;
    pattern = p.pattern;
    minLevel = p.level;
  }

  function togglePause() {
    paused = !paused;
    if (!paused && pending.length) {
      const n = lines.concat(pending);
      lines = n.length > KEEP ? n.slice(n.length - KEEP) : n;
      pending = [];
    }
  }

  async function exportLog() {
    if (!jobId) return;
    const path = await save({ defaultPath: `logcat_${dev?.serial ?? 'device'}_${Date.now()}.txt` });
    if (!path) return;
    try {
      const n = await api.logcatExport(jobId, path);
      pushLog('ok', `${$t('logcat.exported')}: ${n} → ${path}`, 'logcat');
    } catch (e) {
      pushLog('error', errText(e), 'logcat');
    }
  }

  async function clearDevice() {
    if (!dev) return;
    try {
      await api.logcatClear(dev.serial);
      lines = [];
      pushLog('ok', 'adb logcat -c', 'logcat');
    } catch (e) {
      pushLog('error', errText(e), 'logcat');
    }
  }

  const visible = $derived(lines.length > RENDER ? lines.slice(lines.length - RENDER) : lines);

  $effect(() => {
    void visible;
    if (follow && box) requestAnimationFrame(() => box && (box.scrollTop = box.scrollHeight));
  });

  function onScroll() {
    if (!box) return;
    follow = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  }

  const levelClass: Record<string, string> = {
    V: 'text-neutral-500',
    D: 'text-sky-300/80',
    I: 'text-neutral-300',
    W: 'text-warn',
    E: 'text-err',
    F: 'text-err font-bold',
    A: 'text-err font-bold'
  };
</script>

<section class="flex h-[calc(100vh-7.5rem)] animate-fade-in flex-col gap-3">
  <div class="flex flex-wrap items-center justify-between gap-2">
    <div class="flex items-center gap-3">
      <h2 class="view-title">Live Logcat</h2>
      {#if running}<span class="flex items-center gap-1.5 text-xs text-ok"><span class="h-2 w-2 animate-pulse rounded-full bg-ok"></span>{stats.perSec}/s</span>{/if}
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <label class="toggle text-xs" title={$t('logcat.kernelHint')}><input type="checkbox" bind:checked={bufKernel} disabled={running} /><span></span>kernel</label>
      <select class="input !w-auto !py-1.5 text-xs" bind:value={tail} disabled={running} title={$t('logcat.tail')}>
        <option value={0}>0</option><option value={500}>500</option><option value={2000}>2000</option><option value={10000}>10000</option>
      </select>
      {#if running}
        <button class="btn !py-1.5" onclick={togglePause}><Pause size={14} />{paused ? $t('logcat.resume') : $t('logcat.pause')}</button>
        <button class="btn btn-danger !py-1.5" onclick={stop}><Square size={14} />Stop</button>
      {:else}
        <button class="btn btn-gold !py-1.5" disabled={!ready} onclick={start}><Play size={14} />Start</button>
      {/if}
      <button class="icon-btn" title={$t('logcat.export')} disabled={!jobId} onclick={exportLog}><Download size={15} /></button>
      <button class="icon-btn" title={$t('logcat.clearView')} onclick={() => (lines = [])}><Eraser size={15} /></button>
      <button class="icon-btn" title={$t('logcat.clearDevice')} disabled={!ready} onclick={clearDevice}><Trash2 size={15} /></button>
    </div>
  </div>

  <div class="flex flex-wrap items-center gap-2">
    {#each presets as p (p.id)}
      <button class="chip {preset === p.id ? 'chip-on' : ''}" onclick={() => usePreset(p)}>{$t('logcat.p.' + p.id)}</button>
    {/each}
  </div>

  <div class="flex flex-wrap items-center gap-2">
    <div class="relative min-w-[260px] flex-1">
      <Search size={14} class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-neutral-500" />
      <input class="input !pl-9 font-mono text-xs {regexError ? '!border-err' : ''}" placeholder={$t('logcat.regexPh')} bind:value={pattern} oninput={() => (preset = 'custom')} />
    </div>
    <select class="input !w-auto !py-2 text-xs" bind:value={minLevel}>
      <option value="V">≥ Verbose</option><option value="D">≥ Debug</option><option value="I">≥ Info</option>
      <option value="W">≥ Warn</option><option value="E">≥ Error</option><option value="F">Fatal</option>
    </select>
    <label class="check text-xs"><input type="checkbox" bind:checked={ci} />Aa</label>
  </div>
  {#if regexError}<div class="text-xs text-err">{regexError}</div>{/if}

  <div class="relative min-h-0 flex-1">
    <div bind:this={box} onscroll={onScroll} class="h-full overflow-y-auto rounded-lg border border-ink-500 bg-black p-2 font-mono text-[11px] leading-[1.45]">
      {#if !running && lines.length === 0}
        <div class="p-3 text-neutral-600">{ready ? $t('logcat.idle') : $t('common.needAdb')}</div>
      {/if}
      {#each visible as l (l.id)}
        <div class="select-text whitespace-pre-wrap break-all {levelClass[l.level] ?? 'text-neutral-400'}"><span class="text-neutral-600">{l.ts}</span> <span class="text-neutral-600">{l.pid}</span> <span class="font-bold">{l.level}</span> <span class="text-gold/90">{l.tag}</span>: {l.msg}</div>
      {/each}
    </div>
    {#if !follow}
      <button class="btn absolute bottom-3 right-5 !py-1 text-xs" onclick={() => { follow = true; if (box) box.scrollTop = box.scrollHeight; }}>
        <ArrowDownToLine size={13} />{$t('log.follow')}
      </button>
    {/if}
  </div>

  <div class="flex flex-wrap gap-x-5 text-[11px] text-neutral-500">
    <span>{$t('logcat.total')}: <b class="font-mono text-neutral-300">{stats.total}</b></span>
    <span>{$t('logcat.matched')}: <b class="font-mono text-gold">{stats.matched}</b></span>
    <span>{$t('logcat.buffer')}: <b class="font-mono text-neutral-300">{stats.buffered}</b></span>
    {#if dropped}<span class="text-warn">{$t('logcat.dropped')}: {dropped}</span>{/if}
    {#if paused}<span class="text-warn">{$t('logcat.pausedInfo')} ({pending.length})</span>{/if}
    <span>{$t('logcat.showing').replace('{n}', String(visible.length))}</span>
    {#if ended}<span class="text-warn">{ended}</span>{/if}
  </div>
</section>
