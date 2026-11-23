// Wspólny stan aplikacji (Svelte 5 runes) + obsługa zadań w tle.
import { api, type Device, type FinishedData, type LogLevel, type ProgressData, type ToolsInfo, type TransferEvent } from './api';
import { errText } from './format';

// ------------------------------------------------------------------ log --- //

export interface LogEntry {
  id: number;
  level: LogLevel;
  message: string;
  ts: string;
  source: string;
}

let logSeq = 0;
const LOG_LIMIT = 2000;

export const log = $state<{ entries: LogEntry[] }>({ entries: [] });

export function pushLog(level: LogLevel, message: string, source = 'app') {
  const ts = new Date().toLocaleTimeString();
  log.entries.push({ id: ++logSeq, level, message, ts, source });
  if (log.entries.length > LOG_LIMIT) log.entries.splice(0, log.entries.length - LOG_LIMIT);
}

export function clearLog() {
  log.entries = [];
}

// -------------------------------------------------------------- devices --- //

export const app = $state<{
  tools: ToolsInfo | null;
  devices: Device[];
  selected: string | null;
}>({ tools: null, devices: [], selected: null });

export function selectedDevice(): Device | null {
  return app.devices.find((d) => d.serial === app.selected) ?? null;
}

export function setDevices(list: Device[]) {
  const before = new Set(app.devices.map((d) => d.serial + d.mode));
  for (const d of list) {
    if (!before.has(d.serial + d.mode)) pushLog('info', `+ ${d.serial} [${d.mode}] ${d.modelHint ?? ''}`, 'usb');
  }
  const now = new Set(list.map((d) => d.serial + d.mode));
  for (const d of app.devices) {
    if (!now.has(d.serial + d.mode)) pushLog('warn', `- ${d.serial} [${d.mode}]`, 'usb');
  }
  app.devices = list;
  if (!app.selected || !list.some((d) => d.serial === app.selected)) {
    // preferuj urządzenie w pełni dostępne przez ADB
    const best = list.find((d) => d.mode === 'adb') ?? list[0];
    app.selected = best?.serial ?? null;
  }
}

// ----------------------------------------------------------------- jobs --- //

/**
 * Zadanie w tle (pull/push/apps/...). Trzyma postęp, prędkość i ETA.
 * Prędkość liczona z okna ~3 s próbek bajtów -> stabilna, nie skacze.
 */
export class Job {
  id = $state<string | null>(null);
  running = $state(false);
  progress = $state<ProgressData | null>(null);
  result = $state<FinishedData | null>(null);
  speed = $state(0); // B/s
  eta = $state<number | null>(null); // s
  startedAt = $state<number | null>(null);

  #samples: { t: number; b: number }[] = [];
  #source: string;
  onItem: ((id: string, ok: boolean, message: string) => void) | null = null;
  onFinished: ((r: FinishedData) => void) | null = null;

  constructor(source: string) {
    this.#source = source;
  }

  get pct(): number {
    const p = this.progress;
    if (!p) return 0;
    if (p.bytesTotal > 0) return Math.min(100, (p.bytesDone / p.bytesTotal) * 100);
    if (p.total > 0) return Math.min(100, (p.done / p.total) * 100);
    return 0;
  }

  handle = (e: TransferEvent) => {
    switch (e.kind) {
      case 'log':
        pushLog(e.level, e.message, this.#source);
        break;
      case 'progress': {
        this.progress = { done: e.done, total: e.total, bytesDone: e.bytesDone, bytesTotal: e.bytesTotal, current: e.current };
        const now = performance.now();
        this.#samples.push({ t: now, b: e.bytesDone });
        while (this.#samples.length > 2 && now - this.#samples[0].t > 3000) this.#samples.shift();
        const first = this.#samples[0];
        const dt = (now - first.t) / 1000;
        if (dt > 0.4) {
          this.speed = Math.max(0, (e.bytesDone - first.b) / dt);
          this.eta = this.speed > 0 && e.bytesTotal > 0 ? (e.bytesTotal - e.bytesDone) / this.speed : null;
        }
        break;
      }
      case 'item':
        this.onItem?.(e.id, e.ok, e.message);
        break;
      case 'finished':
        this.result = { ok: e.ok, skipped: e.skipped, total: e.total, failures: e.failures, cancelled: e.cancelled };
        this.#done();
        this.onFinished?.(this.result);
        break;
      case 'fatal':
        pushLog('error', e.message, this.#source);
        this.#done();
        break;
    }
  };

  #done() {
    this.running = false;
    this.id = null;
    this.eta = null;
  }

  async start(fn: (onEvent: (e: TransferEvent) => void) => Promise<string>) {
    if (this.running) return;
    this.running = true;
    this.result = null;
    this.progress = null;
    this.speed = 0;
    this.eta = null;
    this.#samples = [];
    this.startedAt = Date.now();
    try {
      this.id = await fn(this.handle);
    } catch (e) {
      pushLog('error', errText(e), this.#source);
      this.#done();
    }
  }

  async cancel() {
    if (this.id) await api.cancelJob(this.id);
  }
}
