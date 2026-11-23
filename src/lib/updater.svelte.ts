// Stan aktualizatora (sprawdzanie w tle, modal, pobieranie).
// Ustawienia w localStorage - tylko wygoda; brak storage = domyślne wartości.
import { api, type UpdateInfo, type UpdateOutcome } from '$lib/api';
import { Job } from '$lib/stores.svelte';
import { errText } from '$lib/format';

const AUTO_KEY = 'adbst.update.auto';
const SKIP_KEY = 'adbst.update.skip';
const REMIND_KEY = 'adbst.update.remindUntil';
const REMIND_MS = 24 * 60 * 60 * 1000;

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, v: string | null) {
  try {
    if (v === null) localStorage.removeItem(key);
    else localStorage.setItem(key, v);
  } catch {
    /* brak storage */
  }
}

class Updater {
  info = $state<UpdateInfo | null>(null);
  checking = $state(false);
  error = $state<string | null>(null);
  open = $state(false);
  outcome = $state<UpdateOutcome | null>(null);
  installError = $state<string | null>(null);
  autoCheck = $state(read(AUTO_KEY) !== '0');
  skipped = $state(read(SKIP_KEY));
  readonly job = new Job('update');

  /** Nowsza wersja, której użytkownik nie pominął. */
  get pending(): boolean {
    return !!this.info?.available && this.info.latest !== this.skipped;
  }

  setAuto(v: boolean) {
    this.autoCheck = v;
    write(AUTO_KEY, v ? '1' : '0');
  }

  /** Ręczne lub automatyczne sprawdzenie. Automatyczne nie zgłasza błędów. */
  async check(manual: boolean): Promise<void> {
    if (this.checking) return;
    this.checking = true;
    this.error = null;
    try {
      this.info = await api.updateCheck();
      if (manual && this.info.available) this.open = true;
      if (!manual && this.pending) {
        const until = Number(read(REMIND_KEY) ?? 0);
        if (Date.now() > until) this.open = true;
      }
    } catch (e) {
      if (manual) this.error = errText(e);
    } finally {
      this.checking = false;
    }
  }

  /** Sprawdzenie w tle kilka sekund po starcie (gdy włączone). */
  scheduleStartupCheck(delayMs = 5000) {
    if (!this.autoCheck) return;
    setTimeout(() => void this.check(false), delayMs);
  }

  skip() {
    if (this.info?.latest) {
      this.skipped = this.info.latest;
      write(SKIP_KEY, this.info.latest);
    }
    this.open = false;
  }

  remindLater() {
    write(REMIND_KEY, String(Date.now() + REMIND_MS));
    this.open = false;
  }

  async install() {
    this.outcome = null;
    this.installError = null;
    await this.job.start((on) =>
      api.updateInstall((e) => {
        if (e.kind === 'fatal') this.installError = e.message;
        if (e.kind === 'item' && e.id === 'result') {
          try {
            this.outcome = JSON.parse(e.message) as UpdateOutcome;
          } catch {
            /* wynik i tak jest w logu */
          }
        }
        on(e);
      })
    );
  }
}

export const updater = new Updater();
