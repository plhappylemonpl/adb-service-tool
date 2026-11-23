<script lang="ts" module>
  // Globalny dialog „Połącz po Wi-Fi”: `openWifi()` / `openWifi('tcpip', serial)`.
  export type WifiMode = 'pair' | 'tcpip';
  const dlg = $state<{ open: boolean; mode: WifiMode; serial: string | null }>({ open: false, mode: 'pair', serial: null });

  export function openWifi(mode: WifiMode = 'pair', serial: string | null = null) {
    dlg.mode = mode;
    dlg.serial = serial;
    dlg.open = true;
  }
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { Wifi, X, LoaderCircle, CircleCheck, CircleX, CircleDashed, Cable, Unplug, Link2, KeyRound, Info } from 'lucide-svelte';
  import { api, type WifiResult, type WifiStage, type WifiStageKey } from '$lib/api';
  import { app, setDevices, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import { toast } from '$lib/components/Toast.svelte';

  const LS_KEY = 'adbst.wifi.last';
  /** Bezpiecznik po stronie GUI - backend ma własne limity (10 s na krok). */
  const UI_WATCHDOG_MS = 75_000;

  // --- formularz parowania ---
  let host = $state('');
  let pairPort = $state('');
  let code = $state('');
  let connectPort = $state('');
  let alreadyPaired = $state(false);

  // --- TCP/IP ---
  let tcpSerial = $state<string | null>(null);
  let tcpPort = $state('5555');

  // --- przebieg ---
  let busy = $state(false);
  let stages = $state<{ key: WifiStageKey; detail: string; st: 'active' | 'done' | 'failed' }[]>([]);
  let error = $state<string | null>(null);
  let result = $state<WifiResult | null>(null);
  let unplugAt = $state<string | null>(null);

  const usbAdb = $derived(app.devices.filter((d) => d.mode === 'adb' && !d.wireless));

  $effect(() => {
    if (!dlg.open) return;
    // tylko otwarcie dialogu jest zależnością - pola formularza czytamy bez śledzenia
    untrack(onOpen);
  });

  function onOpen() {
    // świeże otwarcie: reset przebiegu, ostatni adres z pamięci
    reset();
    try {
      const last = JSON.parse(localStorage.getItem(LS_KEY) ?? 'null');
      if (last && typeof last.host === 'string' && !host) host = last.host;
      if (last && typeof last.connectPort === 'string' && !connectPort) connectPort = last.connectPort;
    } catch {
      /* brak localStorage - pola zostają puste */
    }
    if (dlg.mode === 'tcpip') {
      tcpSerial = dlg.serial ?? usbAdb[0]?.serial ?? null;
    }
  }

  function reset() {
    stages = [];
    error = null;
    result = null;
    unplugAt = null;
  }

  function close() {
    if (busy) return; // w trakcie łączenia dialog jest zablokowany
    dlg.open = false;
  }

  function onKey(e: KeyboardEvent) {
    if (dlg.open && e.key === 'Escape') close();
  }

  /** Wklejenie "192.168.1.20:37215" w pole IP rozbija adres na IP + port. */
  function splitHost(target: 'pair' | 'connect') {
    const m = host.trim().match(/^\[?([0-9a-fA-F:.]+|[A-Za-z0-9.-]+)\]?:(\d{1,5})$/);
    if (!m || (m[1].includes(':') && !host.trim().startsWith('['))) return;
    host = m[1];
    if (target === 'pair' && !alreadyPaired) pairPort = m[2];
    else connectPort = m[2];
  }

  const portOk = (p: string) => /^\d{1,5}$/.test(p) && +p >= 1 && +p <= 65535;
  const hostOk = $derived(/^[A-Za-z0-9.:\-\[\]]{3,}$/.test(host.trim()));
  const codeOk = $derived(/^\d{6}$/.test(code.replace(/[\s-]/g, '')));
  const canPair = $derived(
    hostOk && (alreadyPaired ? portOk(connectPort) : portOk(pairPort) && codeOk && (!connectPort || portOk(connectPort)))
  );
  const canTcp = $derived(!!tcpSerial && portOk(tcpPort));

  function onStage(s: WifiStage) {
    for (const x of stages) if (x.st === 'active' && x.key !== s.stage) x.st = 'done';
    const cur = stages.find((x) => x.key === s.stage);
    if (cur) {
      cur.detail = s.detail || cur.detail;
      cur.st = 'active';
    } else {
      stages.push({ key: s.stage, detail: s.detail, st: 'active' });
    }
    if (s.stage === 'unplug') unplugAt = s.detail;
  }

  async function run(fn: () => Promise<WifiResult>) {
    if (busy) return;
    reset();
    busy = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const watchdog = new Promise<never>((_, rej) => {
        timer = setTimeout(() => rej(new Error($t('wifi.uiTimeout'))), UI_WATCHDOG_MS);
      });
      const r = await Promise.race([fn(), watchdog]);
      for (const x of stages) x.st = 'done';
      result = r;
      pushLog('ok', `Wi-Fi: ${r.serial} [${r.state}]`, 'wifi');
      if (r.state === 'unauthorized') {
        toast('warn', $t('wifi.connected'), $t('wifi.needAuth'));
      } else {
        toast('ok', $t('wifi.connected'), r.address);
      }
      try {
        localStorage.setItem(LS_KEY, JSON.stringify({ host: host.trim(), connectPort }));
      } catch {
        /* zapamiętanie adresu to tylko wygoda */
      }
      try {
        setDevices(await api.scanDevices());
      } catch {
        /* watcher i tak odświeży listę */
      }
      if (app.devices.some((d) => d.serial === r.serial)) app.selected = r.serial;
    } catch (e) {
      const msg = errText(e);
      error = msg;
      for (const x of stages) if (x.st === 'active') x.st = 'failed';
      pushLog('error', `Wi-Fi: ${msg}`, 'wifi');
      toast('error', $t('wifi.failed'), msg);
    } finally {
      clearTimeout(timer);
      busy = false;
    }
  }

  function submitPair() {
    if (!canPair) return;
    const h = host.trim();
    if (alreadyPaired) {
      run(() => {
        onStage({ stage: 'connect', detail: `${h}:${connectPort}` });
        return api.wifiConnect(h, +connectPort);
      });
    } else {
      run(() =>
        api.wifiPair(
          { host: h, pairPort: +pairPort, code: code.replace(/[\s-]/g, ''), connectPort: connectPort ? +connectPort : null },
          onStage
        )
      );
    }
  }

  function submitTcp() {
    if (!canTcp || !tcpSerial) return;
    const s = tcpSerial;
    run(() => api.wifiTcpip(s, +tcpPort, onStage));
  }

  function switchMode(m: 'pair' | 'tcpip') {
    if (busy) return;
    dlg.mode = m;
    reset();
    if (m === 'tcpip' && !tcpSerial) tcpSerial = usbAdb[0]?.serial ?? null;
  }

  const stageIcon = { active: LoaderCircle, done: CircleCheck, failed: CircleX };
</script>

<svelte:window onkeydown={onKey} />

{#if dlg.open}
  <!-- tło: klik zamyka tylko gdy nic się nie dzieje -->
  <div
    class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 p-6 backdrop-blur-sm"
    role="presentation"
    onclick={(e) => {
      if (e.target === e.currentTarget) close();
    }}
  >
    <div
      class="card w-full max-w-xl animate-fade-in space-y-4 border-gold/30 shadow-[0_0_40px_rgba(201,162,73,0.12)]"
      role="dialog"
      aria-modal="true"
      aria-labelledby="wifi-title"
    >
      <div class="flex items-center justify-between gap-3">
        <div class="flex items-center gap-3">
          <div class="flex h-9 w-9 items-center justify-center rounded-lg border border-gold/40 bg-gold/10 text-gold"><Wifi size={18} /></div>
          <div>
            <h2 id="wifi-title" class="text-base font-semibold text-neutral-100">{$t('wifi.title')}</h2>
            <p class="text-xs text-neutral-500">{$t('wifi.subtitle')}</p>
          </div>
        </div>
        <button class="icon-btn" onclick={close} disabled={busy} aria-label={$t('common.cancel')}><X size={18} /></button>
      </div>

      <div class="grid grid-cols-2 gap-1 rounded-lg border border-ink-600 bg-ink-900 p-1">
        <button
          class="flex items-center justify-center gap-2 rounded-md px-3 py-2 text-sm transition-colors {dlg.mode === 'pair'
            ? 'bg-gold/15 text-gold'
            : 'text-neutral-400 hover:text-neutral-200'}"
          onclick={() => switchMode('pair')}
          disabled={busy}
        >
          <KeyRound size={15} />{$t('wifi.modePair')}
        </button>
        <button
          class="flex items-center justify-center gap-2 rounded-md px-3 py-2 text-sm transition-colors {dlg.mode === 'tcpip'
            ? 'bg-gold/15 text-gold'
            : 'text-neutral-400 hover:text-neutral-200'}"
          onclick={() => switchMode('tcpip')}
          disabled={busy}
        >
          <Cable size={15} />{$t('wifi.modeTcp')}
        </button>
      </div>

      {#if dlg.mode === 'pair'}
        <form
          class="flex flex-col gap-3"
          onsubmit={(e) => {
            e.preventDefault();
            submitPair();
          }}
        >
          <p class="flex items-start gap-2 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs text-neutral-400">
            <Info size={14} class="mt-px shrink-0 text-gold" />{alreadyPaired ? $t('wifi.connectHow') : $t('wifi.pairHow')}
          </p>
          <div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
            <div>
              <label class="label" for="wifi-host">{$t('wifi.ip')}</label>
              <input
                id="wifi-host"
                class="input font-mono"
                placeholder="192.168.1.20"
                bind:value={host}
                onblur={() => splitHost(alreadyPaired ? 'connect' : 'pair')}
                onpaste={() => setTimeout(() => splitHost(alreadyPaired ? 'connect' : 'pair'))}
                disabled={busy}
                autocomplete="off"
                spellcheck="false"
              />
            </div>
            {#if !alreadyPaired}
              <div>
                <label class="label" for="wifi-pport">{$t('wifi.pairPort')}</label>
                <input id="wifi-pport" class="input font-mono" placeholder="37215" inputmode="numeric" maxlength="5" bind:value={pairPort} disabled={busy} />
              </div>
            {/if}
          </div>
          {#if !alreadyPaired}
            <div>
              <label class="label" for="wifi-code">{$t('wifi.code')}</label>
              <input
                id="wifi-code"
                class="input text-center font-mono !text-2xl tracking-[0.5em] {code && !codeOk ? '!border-err' : ''}"
                placeholder="••••••"
                inputmode="numeric"
                maxlength="7"
                bind:value={code}
                disabled={busy}
                autocomplete="one-time-code"
              />
            </div>
          {/if}
          <div class="grid gap-3 sm:grid-cols-[9rem_minmax(0,1fr)] sm:items-end">
            <div>
              <label class="label" for="wifi-cport">{$t('wifi.connectPort')}{alreadyPaired ? '' : ` (${$t('wifi.optional')})`}</label>
              <input id="wifi-cport" class="input font-mono" placeholder="41871" inputmode="numeric" maxlength="5" bind:value={connectPort} disabled={busy} />
            </div>
            <p class="pb-2 text-[11px] text-neutral-500">{alreadyPaired ? $t('wifi.connectPortHint2') : $t('wifi.connectPortHint')}</p>
          </div>
          <label class="toggle"><input type="checkbox" bind:checked={alreadyPaired} disabled={busy} /><span></span>{$t('wifi.alreadyPaired')}</label>
          <button class="btn btn-gold w-full justify-center !py-2.5" type="submit" disabled={busy || !canPair}>
            {#if busy}<LoaderCircle size={16} class="animate-spin" />{$t('wifi.connecting')}{:else}<Link2 size={16} />{alreadyPaired
                ? $t('wifi.connect')
                : $t('wifi.pairConnect')}{/if}
          </button>
        </form>
      {:else}
        <div class="flex flex-col gap-3">
          <p class="flex items-start gap-2 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs text-neutral-400">
            <Info size={14} class="mt-px shrink-0 text-gold" />{$t('wifi.tcpHow')}
          </p>
          {#if usbAdb.length === 0}
            <p class="rounded-lg border border-warn/40 bg-warn/10 p-3 text-sm text-warn">{$t('wifi.noUsb')}</p>
          {:else}
            <div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_7rem]">
              <div>
                <label class="label" for="wifi-dev">{$t('wifi.usbDevice')}</label>
                <select id="wifi-dev" class="input" bind:value={tcpSerial} disabled={busy}>
                  {#each usbAdb as d (d.serial)}
                    <option value={d.serial}>{d.modelHint ?? d.serial} ({d.serial})</option>
                  {/each}
                </select>
              </div>
              <div>
                <label class="label" for="wifi-tport">{$t('wifi.port')}</label>
                <input id="wifi-tport" class="input font-mono" inputmode="numeric" maxlength="5" bind:value={tcpPort} disabled={busy} />
              </div>
            </div>
          {/if}
          <button class="btn btn-gold w-full justify-center !py-2.5" onclick={submitTcp} disabled={busy || !canTcp}>
            {#if busy}<LoaderCircle size={16} class="animate-spin" />{$t('wifi.switching')}{:else}<Wifi size={16} />{$t('wifi.switchTcp')}{/if}
          </button>
        </div>
      {/if}

      {#if unplugAt && !error}
        <div class="flex items-center gap-3 rounded-lg border border-gold/50 bg-gold/10 p-3 text-sm text-gold">
          <Unplug size={20} class="shrink-0" />
          <div>
            <div class="font-medium">{$t('wifi.unplug')}</div>
            <div class="font-mono text-xs text-neutral-300">{unplugAt}</div>
          </div>
        </div>
      {/if}

      {#if stages.length}
        <div class="flex flex-col gap-1.5 border-t border-ink-600 pt-3">
          {#each stages as s (s.key)}
            {@const Icon = stageIcon[s.st] ?? CircleDashed}
            <div class="flex items-center gap-2 text-xs {s.st === 'done' ? 'text-ok' : s.st === 'failed' ? 'text-err' : 'text-gold'}">
              <Icon size={14} class={s.st === 'active' ? 'animate-spin' : ''} />
              <span>{$t('wifi.stage.' + s.key)}</span>
              {#if s.detail}<span class="truncate font-mono text-neutral-500">{s.detail}</span>{/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if error}
        <div class="rounded-lg border border-err/40 bg-err/10 p-3 text-sm">
          <div class="mb-1 flex items-center gap-2 font-medium text-err"><CircleX size={16} />{$t('wifi.failed')}</div>
          <p class="select-text whitespace-pre-line text-neutral-200">{error}</p>
        </div>
      {:else if result}
        <div class="flex items-center gap-3 rounded-lg border border-ok/40 bg-ok/10 p-3 text-sm">
          <CircleCheck size={18} class="shrink-0 text-ok" />
          <div class="min-w-0 flex-1">
            <div class="font-medium text-ok">{$t('wifi.connected')}</div>
            <div class="truncate font-mono text-xs text-neutral-300">{result.serial}</div>
            {#if result.state === 'unauthorized'}<div class="mt-1 text-xs text-warn">{$t('wifi.needAuth')}</div>{/if}
          </div>
          <button class="btn !py-1.5" onclick={close}>{$t('wifi.done')}</button>
        </div>
      {/if}
    </div>
  </div>
{/if}
