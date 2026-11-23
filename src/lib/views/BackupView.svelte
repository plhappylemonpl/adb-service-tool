<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import {
    Contact, MessageSquare, HardDrive, Play, Square, Smartphone, LoaderCircle, CircleCheck, CircleX, CircleDashed,
    ShieldCheck, Unlock, Hand, ExternalLink, ChevronRight, TriangleAlert, CloudDownload, CloudUpload, FolderOpen,
    PackageOpen, FileText, MessageSquareReply, Info
  } from 'lucide-svelte';
  import { api, type BackupResult, type RestoreResult, type RestoreDirInfo } from '$lib/api';
  import { selectedDevice, Job } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { basename, humanBytes, errText } from '$lib/format';
  import { toast } from '$lib/components/Toast.svelte';

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && dev.mode === 'adb');

  type Mode = 'backup' | 'restore';
  let mode = $state<Mode>('backup');

  // --- zapamiętane foldery (wygoda, nie wymóg)
  const DEST_KEY = 'adbst.backup.dest';
  const SRC_KEY = 'adbst.restore.src';
  function readKey(k: string): string {
    try {
      return localStorage.getItem(k) ?? '';
    } catch {
      return '';
    }
  }
  function saveKey(k: string, v: string) {
    try {
      if (v) localStorage.setItem(k, v);
    } catch {
      /* zapamiętanie folderu to tylko wygoda */
    }
  }
  let dest = $state(readKey(DEST_KEY));
  let source = $state(readKey(SRC_KEY));
  let waitMinutes = $state(10);
  let restoreWait = $state(30);
  $effect(() => saveKey(DEST_KEY, dest));
  $effect(() => saveKey(SRC_KEY, source));

  // --- podgląd folderu kopii
  let srcInfo = $state<RestoreDirInfo | null>(null);
  let srcError = $state<string | null>(null);
  $effect(() => {
    const dir = source.trim();
    srcInfo = null;
    srcError = null;
    if (!dir) return;
    const timer = setTimeout(async () => {
      try {
        const info = await api.inspectRestoreDir(dir);
        if (dir === source.trim()) srcInfo = info;
      } catch (e) {
        if (dir === source.trim()) srcError = errText(e);
      }
    }, 250);
    return () => clearTimeout(timer);
  });

  // --- etapy
  type StepState = 'pending' | 'active' | 'done' | 'failed';
  const STEPS: Record<Mode, readonly string[]> = {
    backup: ['deploy', 'launch', 'wait', 'pull', 'cleanup'],
    restore: ['deploy', 'push', 'launch', 'waitRestore', 'cleanup']
  };
  const steps = $derived(STEPS[mode]);
  let states = $state<Record<string, StepState>>({});
  let waiting = $state(false);
  let waitStarted = $state(0);
  let elapsed = $state(0);
  let result = $state<BackupResult | null>(null);
  let restoreResult = $state<RestoreResult | null>(null);
  let failure = $state<string | null>(null);

  // --- plan B: ręczna instalacja agenta
  let manualOpen = $state(false);
  let manualReason = $state('');
  let manualPending = $state(false);
  let manualChecking = $state(false);

  const backupJob = new Job('backup');
  const restoreJob = new Job('restore');
  const running = $derived(backupJob.running || restoreJob.running);
  const activeJob = $derived(restoreJob.running ? restoreJob : backupJob);

  function resetSteps() {
    states = Object.fromEntries(steps.map((s) => [s, 'pending' as StepState]));
    result = null;
    restoreResult = null;
    failure = null;
    waiting = false;
    manualOpen = false;
    manualPending = false;
  }

  function activate(key: string) {
    // backend nazywa etap czekania "wait" w obu trybach
    if (key === 'wait' && mode === 'restore') key = 'waitRestore';
    const i = steps.indexOf(key);
    if (i < 0) return;
    steps.forEach((s, j) => {
      if (j < i && states[s] === 'active') states[s] = 'done';
    });
    // sprzątanie po błędzie: poprzednie etapy zostają tak, jak były
    if (key === 'cleanup') {
      for (const s of steps) if (states[s] === 'active') states[s] = failure ? 'failed' : 'done';
      manualOpen = false;
    }
    states[key] = 'active';
    waiting = key === 'wait' || key === 'waitRestore';
    if (waiting) waitStarted = Date.now();
  }

  function onItem(id: string, ok: boolean, message: string) {
    if (id === 'stage:failed') {
      failure = message;
      for (const s of steps) if (states[s] === 'active') states[s] = 'failed';
      waiting = false;
      manualOpen = false;
    } else if (id.startsWith('stage:')) {
      activate(id.slice(6));
    } else if (id === 'manual_install') {
      manualReason = message;
      manualPending = false;
      manualChecking = false;
      manualOpen = true;
    } else if (id === 'manual_install_pending') {
      manualChecking = false;
      manualPending = true;
    } else if (id === 'manual_install_done') {
      manualOpen = false;
      manualChecking = false;
      toast('ok', $t('backup.manual.ok'));
    } else if (id === 'result' && ok) {
      try {
        if (mode === 'restore') restoreResult = JSON.parse(message) as RestoreResult;
        else result = JSON.parse(message) as BackupResult;
      } catch {
        /* wynik i tak jest w logu */
      }
    }
  }
  backupJob.onItem = onItem;
  restoreJob.onItem = onItem;

  function finishedCommon(r: { cancelled: boolean; failures: { error: string }[] }, success: boolean, cancelledKey: string) {
    waiting = false;
    manualOpen = false;
    if (r.cancelled) {
      for (const s of steps) if (states[s] === 'active' || states[s] === 'pending') states[s] = 'pending';
      states.cleanup = 'done';
      toast('warn', $t(cancelledKey));
      return false;
    }
    if (success) {
      for (const s of steps) states[s] = 'done';
      if (r.failures.length) toast('warn', $t('backup.cleanupWarn'), r.failures.map((f) => f.error).join('\n'));
      return true;
    }
    if (states.cleanup === 'active') states.cleanup = 'done';
    return false;
  }

  backupJob.onFinished = (r) => {
    if (finishedCommon(r, !!result, 'backup.cancelled') && result) {
      toast('ok', $t('backup.doneTitle'), `${result.contacts} ${$t('backup.contactsShort')} • ${result.sms} SMS`);
    } else if (!r.cancelled && !result) {
      toast('error', $t('backup.failedTitle'), failure ?? r.failures[0]?.error ?? '');
    }
  };

  restoreJob.onFinished = (r) => {
    const rr = restoreResult;
    if (finishedCommon(r, !!rr, 'restore.cancelled') && rr) {
      toast(
        rr.smsRestored && rr.defaultSmsChanged && !rr.defaultSmsRestored ? 'warn' : 'ok',
        $t('restore.doneTitle'),
        `SMS +${rr.smsInserted} • ${$t('backup.contacts')}: ${rr.contactsCards}`
      );
    } else if (!r.cancelled && !rr) {
      toast('error', $t('restore.failedTitle'), failure ?? r.failures[0]?.error ?? '');
    }
  };

  // licznik czasu oczekiwania w modalu
  $effect(() => {
    if (!waiting) return;
    const iv = setInterval(() => (elapsed = Math.floor((Date.now() - waitStarted) / 1000)), 500);
    return () => clearInterval(iv);
  });

  function switchMode(m: Mode) {
    if (running || m === mode) return;
    mode = m;
    states = {};
    result = null;
    restoreResult = null;
    failure = null;
  }

  async function pickDest() {
    const d = await open({ directory: true, multiple: false });
    if (typeof d === 'string') dest = d;
  }

  async function pickSource() {
    const d = await open({ directory: true, multiple: false, defaultPath: source || dest || undefined });
    if (typeof d === 'string') source = d;
  }

  async function start() {
    if (!dev || !ready || !dest || running) return;
    resetSteps();
    const serial = dev.serial;
    await backupJob.start((on) => api.startContactsBackup(serial, { destDir: dest, waitMinutes }, on));
  }

  async function startRestore() {
    if (!dev || !ready || !srcInfo || running) return;
    resetSteps();
    const serial = dev.serial;
    await restoreJob.start((on) =>
      api.startContactsRestore(serial, { sourceDir: source.trim(), waitMinutes: restoreWait }, on)
    );
  }

  async function confirmManual() {
    if (!dev) return;
    manualChecking = true;
    manualPending = false;
    try {
      const waitingJob = await api.agentInstallConfirm(dev.serial);
      if (!waitingJob) manualChecking = false;
    } catch (e) {
      manualChecking = false;
      toast('error', $t('backup.manual.title'), errText(e));
    }
  }

  const TABS: { m: Mode; label: string; icon: typeof CloudDownload }[] = [
    { m: 'backup', label: 'backup.tabBackup', icon: CloudDownload },
    { m: 'restore', label: 'backup.tabRestore', icon: CloudUpload }
  ];
  const stepIcon = { pending: CircleDashed, active: LoaderCircle, done: CircleCheck, failed: CircleX };
  const fmtTime = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
  const contactsLabel = (r: RestoreResult) =>
    r.contactsMethod === 'intent'
      ? $t('restore.contactsIntent')
      : r.contactsMethod === 'direct'
        ? `${$t('restore.contactsDirect')}: ${r.contactsImported}`
        : $t('restore.contactsNone');
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex flex-wrap items-end justify-between gap-3">
    <div>
      <h2 class="view-title">{$t('backup.title')}</h2>
      <p class="text-xs text-neutral-500">{$t('backup.subtitle')}</p>
    </div>
    <div class="flex rounded-lg border border-ink-600 bg-ink-900 p-1 text-sm" role="tablist">
      {#each TABS as tab (tab.m)}
        {@const m = tab.m}
        {@const Icon = tab.icon}
        <button
          role="tab"
          aria-selected={mode === m}
          disabled={running && mode !== m}
          class="flex items-center gap-2 rounded-md px-3 py-1.5 transition-colors disabled:opacity-40 {mode === m
            ? 'bg-gold/15 text-gold'
            : 'text-neutral-400 hover:text-neutral-200'}"
          onclick={() => switchMode(m)}
        >
          <Icon size={15} />{$t(tab.label)}
        </button>
      {/each}
    </div>
  </div>

  {#if !ready}
    <div class="card text-sm text-warn">{$t('common.needAdb')}</div>
  {:else}
    <div class="grid gap-4 xl:grid-cols-[1fr_340px]">
      <div class="card flex flex-col gap-4">
        <div class="flex items-start gap-3">
          <div class="flex gap-2">
            <div class="flex h-11 w-11 items-center justify-center rounded-lg border border-gold/40 bg-gold/10 text-gold"><Contact size={20} /></div>
            <div class="flex h-11 w-11 items-center justify-center rounded-lg border border-gold/40 bg-gold/10 text-gold"><MessageSquare size={20} /></div>
          </div>
          <div class="text-sm text-neutral-300">
            {#if mode === 'backup'}
              <p>{$t('backup.how')}</p>
            {:else}
              <p>{$t('restore.how')}</p>
              <p class="mt-2 flex items-start gap-1.5 text-xs text-neutral-400"><MessageSquareReply size={14} class="mt-px shrink-0 text-gold" />{$t('restore.smsNote')}</p>
            {/if}
            <p class="mt-2 flex items-start gap-1.5 text-xs text-neutral-500"><ShieldCheck size={14} class="mt-px shrink-0 text-ok" />{$t('backup.privacy')}</p>
          </div>
        </div>

        <ol class="flex flex-col gap-2 border-t border-ink-600 pt-4">
          {#each steps as s, i (s)}
            {@const st = states[s] ?? 'pending'}
            {@const Icon = stepIcon[st]}
            <li class="flex items-center gap-3 text-sm {st === 'done' ? 'text-ok' : st === 'active' ? 'text-gold' : st === 'failed' ? 'text-err' : 'text-neutral-500'}">
              <Icon size={16} class={st === 'active' ? 'animate-spin' : ''} />
              <span class="w-5 text-right font-mono text-xs opacity-60">{i + 1}.</span>
              <span>{$t('backup.step.' + s)}</span>
            </li>
          {/each}
        </ol>

        {#if failure}
          <div class="rounded-lg border border-err/40 bg-err/10 p-3 text-sm">
            <div class="mb-1 flex items-center gap-2 font-medium text-err">
              <CircleX size={16} />{$t(mode === 'restore' ? 'restore.failedTitle' : 'backup.failedTitle')}
            </div>
            <p class="select-text whitespace-pre-wrap text-neutral-200">{failure}</p>
            <p class="mt-2 text-xs text-neutral-500">{$t(mode === 'restore' ? 'restore.failedNote' : 'backup.failedNote')}</p>
          </div>
        {/if}

        {#if mode === 'backup' && result}
          <div class="rounded-lg border border-ok/40 bg-ok/10 p-4">
            <div class="mb-3 flex items-center gap-2 font-medium text-ok"><CircleCheck size={18} />{$t('backup.doneTitle')}</div>
            <div class="grid grid-cols-2 gap-3 text-sm">
              <div class="rounded-lg border border-ink-600 bg-ink-900 p-3">
                <div class="flex items-center gap-2 text-neutral-400"><Contact size={15} />{$t('backup.contacts')}</div>
                <div class="mt-1 font-mono text-2xl text-gold">{result.contactsDenied ? '—' : result.contacts}</div>
                {#if result.contactsDenied}<div class="text-xs text-warn">{$t('backup.denied')}</div>{/if}
              </div>
              <div class="rounded-lg border border-ink-600 bg-ink-900 p-3">
                <div class="flex items-center gap-2 text-neutral-400"><MessageSquare size={15} />SMS</div>
                <div class="mt-1 font-mono text-2xl text-gold">{result.smsDenied ? '—' : result.sms}</div>
                {#if result.smsDenied}<div class="text-xs text-warn">{$t('backup.denied')}</div>{/if}
              </div>
            </div>
            <div class="mt-3 space-y-1 text-xs text-neutral-400">
              {#each result.files as f (f)}<div class="truncate font-mono" title={f}>{basename(f)}</div>{/each}
            </div>
            <div class="mt-3 flex flex-wrap gap-2">
              <button class="btn !py-1.5" onclick={() => api.revealPath(result!.files[0] ?? result!.dir)}>
                <ExternalLink size={14} />{$t('backup.openFolder')}
              </button>
              <button class="btn !py-1.5" onclick={() => { source = result!.dir; switchMode('restore'); }}>
                <CloudUpload size={14} />{$t('backup.tabRestore')}
              </button>
            </div>
          </div>
        {/if}

        {#if mode === 'restore' && restoreResult}
          {@const rr = restoreResult}
          <div class="rounded-lg border border-ok/40 bg-ok/10 p-4">
            <div class="mb-3 flex items-center gap-2 font-medium text-ok"><CircleCheck size={18} />{$t('restore.doneTitle')}</div>
            <div class="grid grid-cols-2 gap-3 text-sm">
              <div class="rounded-lg border border-ink-600 bg-ink-900 p-3">
                <div class="flex items-center gap-2 text-neutral-400"><Contact size={15} />{$t('backup.contacts')}</div>
                <div class="mt-1 font-mono text-2xl text-gold">{rr.contactsFile ? rr.contactsCards : '—'}</div>
                {#if rr.contactsFile}
                  <div class="text-xs {rr.contactsMethod === 'none' ? 'text-warn' : 'text-neutral-400'}">{contactsLabel(rr)}</div>
                {/if}
              </div>
              <div class="rounded-lg border border-ink-600 bg-ink-900 p-3">
                <div class="flex items-center gap-2 text-neutral-400"><MessageSquare size={15} />SMS</div>
                <div class="mt-1 font-mono text-2xl text-gold">{rr.smsRestored ? `+${rr.smsInserted}` : '—'}</div>
                {#if rr.smsRestored}
                  <div class="text-xs text-neutral-400">
                    {rr.smsInserted} {$t('restore.smsNew')} • {rr.smsSkipped} {$t('restore.smsSkipped')}
                    {#if rr.smsFailed > 0}<span class="text-warn"> • {rr.smsFailed} {$t('restore.smsFailed')}</span>{/if}
                  </div>
                {:else if rr.smsFile}
                  <div class="text-xs text-warn">{$t('restore.smsNone')}</div>
                {/if}
              </div>
            </div>
            <ul class="mt-3 space-y-1.5 text-xs">
              {#if rr.defaultSmsChanged || rr.smsRestored}
                {#if rr.defaultSmsRestored}
                  <li class="flex items-center gap-1.5 text-ok"><CircleCheck size={13} />{$t('restore.defaultOk')}{rr.previousSmsApp ? ` (${rr.previousSmsApp})` : ''}</li>
                {:else}
                  <li class="flex items-start gap-1.5 text-warn"><TriangleAlert size={13} class="mt-px shrink-0" />{$t('restore.defaultBad')}</li>
                {/if}
              {/if}
              {#if rr.mmsSkipped > 0}
                <li class="flex items-center gap-1.5 text-neutral-400"><Info size={13} />{rr.mmsSkipped} {$t('restore.mms')}</li>
              {/if}
              {#if rr.incomingSmsSaved > 0}
                <li class="flex items-center gap-1.5 text-neutral-400"><Info size={13} />{$t('restore.incoming')}: {rr.incomingSmsSaved}</li>
              {/if}
              {#if rr.manualInstall}
                <li class="flex items-center gap-1.5 text-neutral-400"><PackageOpen size={13} />{$t('restore.manual')}</li>
              {/if}
            </ul>
          </div>
        {/if}
      </div>

      <div class="space-y-4">
        <div class="card space-y-3">
          {#if mode === 'backup'}
            <div>
              <span class="label">{$t('pull.dest')}</span>
              <div class="flex gap-2">
                <input class="input flex-1" bind:value={dest} placeholder={$t('pull.destPh')} disabled={running} />
                <button class="btn !px-3" onclick={pickDest} title={$t('pull.pickDest')} disabled={running}><HardDrive size={15} /></button>
              </div>
            </div>
            <label class="block">
              <span class="label">{$t('backup.wait')}</span>
              <input type="number" min="1" max="60" bind:value={waitMinutes} class="input" disabled={running} />
            </label>
          {:else}
            <div>
              <span class="label">{$t('restore.source')}</span>
              <div class="flex gap-2">
                <input class="input flex-1" bind:value={source} placeholder={$t('restore.sourcePh')} disabled={running} />
                <button class="btn !px-3" onclick={pickSource} title={$t('restore.pick')} disabled={running}><FolderOpen size={15} /></button>
              </div>
            </div>
            {#if srcInfo}
              <div class="space-y-1.5 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs">
                <div class="text-neutral-500">{$t('restore.files')}</div>
                {#each [{ info: srcInfo.contacts, Icon: Contact }, { info: srcInfo.sms, Icon: MessageSquare }] as row, i (i)}
                  {#if row.info}
                    {@const info = row.info}
                    {@const Icon = row.Icon}
                    <div class="flex items-center gap-2 text-neutral-300" title={info.path}>
                      <Icon size={13} class="shrink-0 text-gold" />
                      <span class="truncate font-mono">{basename(info.path)}</span>
                      <span class="ml-auto shrink-0 text-neutral-500">{humanBytes(info.size)}</span>
                    </div>
                  {/if}
                {/each}
              </div>
            {:else if srcError}
              <div class="flex items-start gap-1.5 rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-xs text-warn">
                <FileText size={13} class="mt-px shrink-0" />{$t('restore.noFiles')}
              </div>
            {/if}
            <label class="block">
              <span class="label">{$t('restore.wait')}</span>
              <input type="number" min="5" max="120" bind:value={restoreWait} class="input" disabled={running} />
            </label>
          {/if}
          <div class="flex items-center gap-2 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs text-neutral-400">
            <Smartphone size={14} class="shrink-0 text-gold" />
            <span class="truncate">{dev?.modelHint ?? dev?.serial}</span>
          </div>
        </div>
        {#if running}
          <button class="btn btn-danger w-full justify-center py-3" onclick={() => activeJob.cancel()}>
            <Square size={16} />{$t('common.cancel')}
          </button>
        {:else if mode === 'backup'}
          <button class="btn btn-gold w-full justify-center py-3 text-base" disabled={!dest} onclick={start}>
            <Play size={17} />{$t('backup.start')}
          </button>
        {:else}
          <button class="btn btn-gold w-full justify-center py-3 text-base" disabled={!srcInfo} onclick={startRestore}>
            <CloudUpload size={17} />{$t('restore.start')}
          </button>
        {/if}
        <p class="flex items-start gap-1.5 text-[11px] text-neutral-500">
          <TriangleAlert size={13} class="mt-px shrink-0 text-warn" />{$t('backup.xiaomiNote')}
        </p>
        {#if mode === 'restore'}
          <p class="flex items-start gap-1.5 text-[11px] text-neutral-500">
            <Info size={13} class="mt-px shrink-0 text-gold" />{$t('restore.restricted')}
          </p>
        {/if}
      </div>
    </div>
  {/if}
</section>

<!-- Modal: plan B - ręczna instalacja Agent.apk z folderu Pobrane -->
{#if manualOpen && running}
  <div class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 p-6 backdrop-blur-sm" role="presentation">
    <div class="card w-full max-w-lg animate-fade-in space-y-5 border-warn/50 shadow-[0_0_40px_rgba(217,180,102,0.15)]" role="dialog" aria-modal="true">
      <div class="flex items-center gap-3">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl border border-warn/50 bg-warn/10 text-warn">
          <PackageOpen size={24} />
        </div>
        <div>
          <h3 class="text-base font-semibold text-neutral-100">{$t('backup.manual.title')}</h3>
          <p class="text-xs text-neutral-500">{dev?.modelHint ?? dev?.serial}</p>
        </div>
      </div>

      <p class="text-sm leading-relaxed text-neutral-200">{$t('backup.manual.text')}</p>

      <ol class="space-y-2 text-sm text-neutral-300">
        <li class="flex items-start gap-3"><FolderOpen size={17} class="mt-0.5 shrink-0 text-gold" />{$t('backup.manual.tip1')}</li>
        <li class="flex items-start gap-3"><ShieldCheck size={17} class="mt-0.5 shrink-0 text-gold" />{$t('backup.manual.tip2')}</li>
        <li class="flex items-start gap-3"><Info size={17} class="mt-0.5 shrink-0 text-gold" />{$t('backup.manual.tip3')}</li>
      </ol>

      {#if manualReason}
        <p class="select-text rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs text-neutral-400">
          <span class="text-neutral-500">{$t('backup.manual.reason')}:</span> {manualReason}
        </p>
      {/if}

      {#if manualPending}
        <div class="flex items-start gap-2 rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-xs text-warn">
          <TriangleAlert size={14} class="mt-px shrink-0" />{$t('backup.manual.pending')}
        </div>
      {/if}

      <div class="flex items-center justify-between gap-3 border-t border-ink-600 pt-4">
        <span class="flex items-center gap-2 text-xs text-neutral-400">
          <LoaderCircle size={14} class="animate-spin text-gold" />{$t('backup.manual.auto')}
        </span>
        <div class="flex gap-2">
          <button class="btn btn-danger !py-1.5" onclick={() => activeJob.cancel()}><Square size={14} />{$t('common.cancel')}</button>
          <button class="btn btn-gold !py-1.5" disabled={manualChecking} onclick={confirmManual}>
            {#if manualChecking}<LoaderCircle size={14} class="animate-spin" />{$t('backup.manual.checking')}{:else}<CircleCheck size={14} />{$t('backup.manual.done')}{/if}
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}

<!-- Modal: czekamy na użytkownika przy urządzeniu (nie da się go zamknąć klikiem w tło) -->
{#if waiting && running}
  <div class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 p-6 backdrop-blur-sm" role="presentation">
    <div class="card w-full max-w-lg animate-fade-in space-y-5 border-gold/40 shadow-[0_0_40px_rgba(201,162,73,0.15)]" role="dialog" aria-modal="true">
      <div class="flex items-center gap-3">
        <div class="relative flex h-12 w-12 items-center justify-center rounded-xl border border-gold/50 bg-gold/10 text-gold">
          <Smartphone size={24} />
          <span class="absolute -right-1 -top-1 h-3 w-3 animate-ping rounded-full bg-gold"></span>
        </div>
        <div>
          <h3 class="text-base font-semibold text-neutral-100">{$t(mode === 'restore' ? 'restore.modalTitle' : 'backup.modalTitle')}</h3>
          <p class="text-xs text-neutral-500">{dev?.modelHint ?? dev?.serial}</p>
        </div>
      </div>

      {#if mode === 'restore'}
        <ol class="space-y-3 text-sm text-neutral-200">
          <li class="flex items-start gap-3"><Unlock size={18} class="mt-0.5 shrink-0 text-gold" />{$t('restore.modal1')}</li>
          <li class="flex items-start gap-3"><Contact size={18} class="mt-0.5 shrink-0 text-gold" />{$t('restore.modal2')}</li>
          <li class="flex items-start gap-3"><MessageSquare size={18} class="mt-0.5 shrink-0 text-gold" />{$t('restore.modal3')}</li>
          <li class="flex items-start gap-3"><MessageSquareReply size={18} class="mt-0.5 shrink-0 text-gold" />{$t('restore.modal4')}</li>
        </ol>
      {:else}
        <ol class="space-y-3 text-sm text-neutral-200">
          <li class="flex items-start gap-3"><Unlock size={18} class="mt-0.5 shrink-0 text-gold" />{$t('backup.modal1')}</li>
          <li class="flex items-start gap-3"><ShieldCheck size={18} class="mt-0.5 shrink-0 text-gold" />{$t('backup.modal2')}</li>
          <li class="flex items-start gap-3"><Hand size={18} class="mt-0.5 shrink-0 text-gold" />{$t('backup.modal3')}</li>
        </ol>
      {/if}

      <div class="flex items-center justify-between gap-3 border-t border-ink-600 pt-4">
        <span class="flex items-center gap-2 text-xs text-neutral-400">
          <LoaderCircle size={14} class="animate-spin text-gold" />{$t('backup.polling')}
          <span class="font-mono text-neutral-300">{fmtTime(elapsed)}</span>
          <ChevronRight size={12} /><span class="font-mono">{mode === 'restore' ? restoreWait : waitMinutes}:00</span>
        </span>
        <button class="btn btn-danger !py-1.5" onclick={() => activeJob.cancel()}><Square size={14} />{$t('common.cancel')}</button>
      </div>
    </div>
  </div>
{/if}
