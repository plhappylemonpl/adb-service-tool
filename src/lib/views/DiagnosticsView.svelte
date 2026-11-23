<script lang="ts">
  import { Activity, Play, Square, BatteryCharging, BatteryMedium, Thermometer, Zap, Gauge, RotateCw, HeartPulse, PlugZap } from 'lucide-svelte';
  import { api, type DiagSnapshot } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import Sparkline from '$lib/components/Sparkline.svelte';

  let { active = true }: { active?: boolean } = $props();

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && (dev.mode === 'adb' || dev.mode === 'recovery'));

  const HISTORY = 120;
  let snap = $state<DiagSnapshot | null>(null);
  let volt = $state<number[]>([]);
  let curr = $state<number[]>([]);
  let temp = $state<number[]>([]);
  let jobId = $state<string | null>(null);
  let running = $state(false);
  let intervalMs = $state(1000);
  let autoStart = $state(true);
  let lastError = $state<string | null>(null);

  function push(arr: number[], v: number | null | undefined): number[] {
    if (v === null || v === undefined || !Number.isFinite(v)) return arr;
    const n = [...arr, v];
    return n.length > HISTORY ? n.slice(n.length - HISTORY) : n;
  }

  async function start() {
    if (!dev || running) return;
    running = true;
    lastError = null;
    volt = [];
    curr = [];
    temp = [];
    try {
      jobId = await api.diagStart(dev.serial, intervalMs, (e) => {
        if (e.kind === 'snapshot') {
          snap = e.data;
          lastError = null;
          volt = push(volt, e.data.battery.voltageMv);
          curr = push(curr, e.data.battery.currentMa);
          temp = push(temp, e.data.battery.tempC);
        } else if (e.kind === 'error') {
          lastError = e.message;
        } else if (e.kind === 'stopped') {
          running = false;
          jobId = null;
          if (e.reason !== 'stop') pushLog('warn', e.reason, 'diag');
        }
      });
    } catch (e) {
      running = false;
      pushLog('error', errText(e), 'diag');
    }
  }

  async function stop() {
    if (jobId) await api.cancelJob(jobId).catch(() => {});
    running = false;
    jobId = null;
  }

  // auto start/stop z widocznością zakładki i zmianą urządzenia
  let lastSerial: string | null = null;
  $effect(() => {
    const s = ready && dev ? dev.serial : null;
    const want = active && autoStart && !!s;
    if (s !== lastSerial && running) {
      stop();
    }
    lastSerial = s;
    if (want && !running) start();
    if (!want && running && !active) stop();
  });

  const b = $derived(snap?.battery);
  const charging = $derived((b?.status ?? '').toLowerCase() === 'charging');
  const hottest = $derived(snap?.thermal[0]?.tempC ?? null);

  function tempColor(c: number): string {
    if (c >= 70) return '#e08a8a';
    if (c >= 50) return '#d9b466';
    return '#7fd08a';
  }
  function fmt(v: number | null | undefined, digits = 0, unit = ''): string {
    return v === null || v === undefined ? '—' : `${v.toFixed(digits)}${unit}`;
  }
  const na = $derived((k: string) => snap?.unsupported.includes(k) ?? false);
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex flex-wrap items-center justify-between gap-3">
    <div class="flex items-center gap-3">
      <h2 class="view-title">{$t('diag.title')}</h2>
      {#if running}<span class="flex items-center gap-1.5 text-xs text-ok"><span class="h-2 w-2 animate-pulse rounded-full bg-ok"></span>LIVE</span>{/if}
    </div>
    <div class="flex items-center gap-2">
      <select class="input !w-auto !py-1.5 text-xs" bind:value={intervalMs} disabled={running}>
        <option value={500}>0.5 s</option>
        <option value={1000}>1 s</option>
        <option value={2000}>2 s</option>
        <option value={5000}>5 s</option>
      </select>
      <label class="toggle text-xs"><input type="checkbox" bind:checked={autoStart} /><span></span>auto</label>
      {#if running}
        <button class="btn btn-danger" onclick={stop}><Square size={15} />{$t('diag.stop')}</button>
      {:else}
        <button class="btn btn-gold" disabled={!ready} onclick={start}><Play size={15} />{$t('diag.start')}</button>
      {/if}
    </div>
  </div>

  {#if !ready}
    <div class="card text-sm text-warn">{$t('common.needAdb')}</div>
  {:else if !snap}
    <div class="card animate-pulse text-sm text-neutral-500">{running ? $t('common.loading') : $t('diag.idle')}</div>
  {/if}
  {#if lastError}<div class="card border-err/40 text-xs text-err">{lastError}</div>{/if}

  {#if snap && b}
    <!-- kafelki -->
    <div class="grid grid-cols-2 gap-3 lg:grid-cols-4">
      <div class="card flex items-center gap-4">
        <div class="relative h-20 w-20 shrink-0">
          <svg viewBox="0 0 36 36" class="h-20 w-20 -rotate-90">
            <circle cx="18" cy="18" r="15.9" fill="none" stroke="#222" stroke-width="3.2" />
            <circle cx="18" cy="18" r="15.9" fill="none" stroke={(b.capacity ?? 0) < 20 ? '#e08a8a' : '#c9a249'} stroke-width="3.2"
              stroke-dasharray="{b.capacity ?? 0} 100" stroke-linecap="round" class="transition-all duration-500" />
          </svg>
          <div class="absolute inset-0 flex items-center justify-center font-mono text-lg font-bold text-neutral-100">{fmt(b.capacity, 0, '%')}</div>
        </div>
        <div>
          <div class="flex items-center gap-1.5 text-xs text-neutral-500">
            {#if charging}<BatteryCharging size={14} class="text-ok" />{:else}<BatteryMedium size={14} />{/if}{$t('diag.capacity')}
          </div>
          <div class="text-sm text-neutral-200">{b.status ?? '—'}</div>
          <div class="text-[11px] text-neutral-500">{b.technology ?? ''} {b.chargeType && b.chargeType !== 'N/A' ? '• ' + b.chargeType : ''}</div>
        </div>
      </div>

      <div class="card">
        <div class="flex items-center gap-1.5 text-xs text-neutral-500"><RotateCw size={14} />{$t('diag.cycles')}</div>
        <div class="font-mono text-3xl font-bold {na('cycleCount') ? 'text-neutral-600' : 'text-gold'}">{b.cycleCount ?? '—'}</div>
        <div class="text-[11px] text-neutral-500">{na('cycleCount') ? $t('diag.unsupported') : $t('diag.cyclesHint')}</div>
      </div>

      <div class="card">
        <div class="flex items-center gap-1.5 text-xs text-neutral-500"><HeartPulse size={14} />{$t('diag.health')}</div>
        <div class="font-mono text-3xl font-bold {b.healthPct === null ? 'text-neutral-600' : b.healthPct < 80 ? 'text-err' : b.healthPct < 90 ? 'text-warn' : 'text-ok'}">
          {fmt(b.healthPct, 1, '%')}
        </div>
        <div class="text-[11px] text-neutral-500">
          {#if b.chargeFullMah && b.chargeFullDesignMah}{fmt(b.chargeFullMah, 0)} / {fmt(b.chargeFullDesignMah, 0)} mAh{:else}{$t('diag.unsupported')}{/if}
          • {b.health ?? '—'}
        </div>
      </div>

      <div class="card">
        <div class="flex items-center gap-1.5 text-xs text-neutral-500"><PlugZap size={14} />{$t('diag.power')}</div>
        <div class="font-mono text-3xl font-bold {(b.powerW ?? 0) > 0 ? 'text-ok' : 'text-neutral-200'}">{fmt(b.powerW, 2, ' W')}</div>
        <div class="text-[11px] text-neutral-500">
          {#each snap.sources.filter((s) => s.online) as s (s.name)}<span class="mr-1">{s.name}</span>{:else}{$t('diag.onBattery')}{/each}
        </div>
      </div>
    </div>

    <!-- wykresy -->
    <div class="grid gap-3 lg:grid-cols-3">
      <div class="card space-y-1">
        <div class="flex items-center justify-between text-xs"><span class="flex items-center gap-1.5 text-neutral-500"><Zap size={14} />{$t('diag.voltage')}</span>
          <span class="font-mono text-lg text-gold">{fmt(b.voltageMv, 0, ' mV')}</span></div>
        <Sparkline values={volt} />
      </div>
      <div class="card space-y-1">
        <div class="flex items-center justify-between text-xs"><span class="flex items-center gap-1.5 text-neutral-500"><Gauge size={14} />{$t('diag.current')}</span>
          <span class="font-mono text-lg {(b.currentMa ?? 0) >= 0 ? 'text-ok' : 'text-sky-300'}">{fmt(b.currentMa, 0, ' mA')}</span></div>
        <Sparkline values={curr} color={(b.currentMa ?? 0) >= 0 ? '#7fd08a' : '#7dd3fc'} />
        {#if na('currentMa')}<div class="text-[11px] text-neutral-600">{$t('diag.unsupported')}</div>{/if}
      </div>
      <div class="card space-y-1">
        <div class="flex items-center justify-between text-xs"><span class="flex items-center gap-1.5 text-neutral-500"><Thermometer size={14} />{$t('diag.batTemp')}</span>
          <span class="font-mono text-lg" style="color:{tempColor(b.tempC ?? 0)}">{fmt(b.tempC, 1, ' °C')}</span></div>
        <Sparkline values={temp} color={tempColor(b.tempC ?? 0)} />
      </div>
    </div>

    <!-- czujniki temperatury -->
    <div class="card">
      <div class="mb-2 flex items-center justify-between text-sm">
        <span class="flex items-center gap-2 text-gold"><Activity size={15} />{$t('diag.thermal')} ({snap.thermal.length})</span>
        {#if hottest !== null}<span class="text-xs text-neutral-500">max: <b class="font-mono" style="color:{tempColor(hottest)}">{hottest.toFixed(1)} °C</b></span>{/if}
      </div>
      {#if snap.thermal.length}
        <div class="grid gap-x-6 gap-y-1.5 md:grid-cols-2 2xl:grid-cols-3">
          {#each snap.thermal as z, i (z.name + i)}
            <div class="flex items-center gap-2 text-xs">
              <span class="w-36 shrink-0 truncate font-mono text-neutral-400" title={z.name}>{z.name}</span>
              <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-ink-600">
                <div class="h-full rounded-full transition-all duration-500" style="width:{Math.min(100, Math.max(2, z.tempC))}%;background:{tempColor(z.tempC)}"></div>
              </div>
              <span class="w-14 shrink-0 text-right font-mono" style="color:{tempColor(z.tempC)}">{z.tempC.toFixed(1)}°</span>
            </div>
          {/each}
        </div>
      {:else}
        <div class="text-sm text-neutral-600">{$t('diag.noThermal')}</div>
      {/if}
    </div>
    <p class="text-[11px] text-neutral-600">{$t('diag.source')}: {b.source}. {$t('diag.note')}</p>
  {/if}
</section>
