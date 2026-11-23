<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import {
    FileCode2, Wrench, Smartphone, FolderOpen, FileArchive, Play, Zap, Square, RefreshCw, Trash2, CircleCheck,
    CircleX, CircleDashed, LoaderCircle, ChevronRight, ChevronDown, Folder, FileImage, FileText, File, Undo2,
    Replace, Download, Eye, Hammer, Bug, Copy, Tag, TriangleAlert, ShieldCheck, Search, Layers, ExternalLink
  } from 'lucide-svelte';
  import {
    api, type AppEntry, type BuildOptions, type BuildResult, type DevWorkspace, type PatchOptions,
    type ResEntry, type ResPreview, type ToolchainStatus, type TransferEvent, type WorkspaceMeta, type OpenSource
  } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText, humanBytes, basename } from '$lib/format';
  import { confirmDialog } from '$lib/components/Confirm.svelte';

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && dev.mode === 'adb');
  const PKG_RE = /^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z][A-Za-z0-9_]*)+$/;

  // ------------------------------------------------------------------ //
  // Zadanie łańcucha (etapy + płynny pasek)
  // ------------------------------------------------------------------ //
  type StepState = 'pending' | 'active' | 'done' | 'failed';

  class Pipeline {
    id = $state<string | null>(null);
    running = $state(false);
    steps = $state<string[]>([]);
    states = $state<Record<string, StepState>>({});
    permille = $state(0);
    detail = $state('');
    dl = $state<{ done: number; total: number } | null>(null);
    error = $state<string | null>(null);
    failedStep = $state<string | null>(null);
    cancelled = $state(false);
    ended = $state(false);
    result = $state<BuildResult | null>(null);
    onWorkspace: ((id: string) => void) | null = null;
    onEnd: (() => void) | null = null;

    #activate(key: string) {
      const i = this.steps.indexOf(key);
      if (i < 0) return;
      this.steps.forEach((s, j) => {
        if (j < i && this.states[s] !== 'failed') this.states[s] = 'done';
      });
      if (this.states[key] === 'pending') this.states[key] = 'active';
    }

    handle = (e: TransferEvent) => {
      switch (e.kind) {
        case 'log':
          pushLog(e.level, e.message, 'apk');
          break;
        case 'progress':
          if (e.bytesTotal > 0) {
            // pobieranie narzędzi (JRE/jar) - osobny, dokładny pasek
            this.dl = { done: e.bytesDone, total: e.bytesTotal };
          } else {
            this.permille = e.done;
            const [key, detail] = (e.current ?? '').split('|');
            if (key && key !== 'done') this.#activate(key);
            this.detail = detail ?? '';
            if (key !== 'tools') this.dl = null;
          }
          break;
        case 'item':
          if (e.id.startsWith('step:')) {
            const k = e.id.slice(5);
            this.states[k] = e.ok ? 'done' : 'failed';
            if (!e.ok) {
              this.error = e.message;
              this.failedStep = k;
            }
          } else if (e.id === 'workspace' && e.ok) {
            this.onWorkspace?.(e.message);
          } else if (e.id === 'result' && e.ok) {
            try {
              this.result = JSON.parse(e.message) as BuildResult;
            } catch {
              /* niepoprawny JSON - wynik i tak w logu */
            }
          }
          break;
        case 'finished':
          this.cancelled = e.cancelled;
          if (!e.cancelled && e.failures.length === 0) {
            this.permille = 1000;
            this.steps.forEach((s) => (this.states[s] = 'done'));
          } else if (e.failures.length) {
            this.error ??= e.failures[0].error;
          }
          this.#end();
          break;
        case 'fatal':
          pushLog('error', e.message, 'apk');
          this.error = e.message;
          this.#end();
          break;
      }
    };

    #end() {
      this.running = false;
      this.ended = true;
      this.id = null;
      this.dl = null;
      for (const s of this.steps) if (this.states[s] === 'active') this.states[s] = this.error ? 'failed' : 'pending';
      this.onEnd?.();
    }

    async start(steps: string[], fn: (on: (e: TransferEvent) => void) => Promise<string>) {
      if (this.running) return;
      this.steps = steps;
      this.states = Object.fromEntries(steps.map((s) => [s, 'pending' as StepState]));
      this.permille = 0;
      this.detail = '';
      this.dl = null;
      this.error = null;
      this.failedStep = null;
      this.cancelled = false;
      this.ended = false;
      this.result = null;
      this.running = true;
      try {
        this.id = await fn(this.handle);
      } catch (e) {
        this.error = errText(e);
        pushLog('error', this.error, 'apk');
        this.#end();
      }
    }

    async cancel() {
      if (this.id) await api.cancelJob(this.id);
    }
  }

  const job = new Pipeline();

  // ------------------------------------------------------------------ //
  // Narzędzia
  // ------------------------------------------------------------------ //
  let tools = $state<ToolchainStatus | null>(null);

  async function refreshTools() {
    try {
      tools = await api.devtoolsStatus();
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  function setupTools() {
    job.onEnd = refreshTools;
    job.start(['tools'], (on) => api.devtoolsSetup(on));
  }

  async function importFramework() {
    const f = await open({ multiple: false, filters: [{ name: 'Framework APK', extensions: ['apk'] }] });
    if (typeof f !== 'string') return;
    try {
      const out = await api.devtoolsImportFramework(f);
      pushLog('ok', `${$t('dt.fwImported')}: ${basename(f)}${out ? ' — ' + out : ''}`, 'apk');
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  // ------------------------------------------------------------------ //
  // Źródło APK
  // ------------------------------------------------------------------ //
  let srcMode = $state<'device' | 'file'>('device');
  let apps = $state<AppEntry[]>([]);
  let appsLoading = $state(false);
  let appsFor = $state<string | null>(null);
  let appFilter = $state('');
  let showSystem = $state(false);
  let pickedPkg = $state<string | null>(null);
  let filePath = $state<string | null>(null);

  // automat - opcje wbudowane w kartę źródła
  let autoDebuggable = $state(true);
  let autoClone = $state(false);
  let autoPkg = $state('');
  let autoUninstall = $state(false);

  const visibleApps = $derived.by(() => {
    const q = appFilter.trim().toLowerCase();
    return apps
      .filter((a) => a.installed && (showSystem || !a.system))
      .filter((a) => !q || a.package.toLowerCase().includes(q))
      .slice(0, 400);
  });

  async function loadApps() {
    if (!dev || !ready) return;
    appsLoading = true;
    try {
      apps = (await api.listApps(dev.serial)).sort((a, b) => a.package.localeCompare(b.package));
      appsFor = dev.serial;
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    } finally {
      appsLoading = false;
    }
  }

  $effect(() => {
    if (srcMode === 'device' && ready && dev && appsFor !== dev.serial && !appsLoading) loadApps();
  });

  $effect(() => {
    if (pickedPkg && !autoPkg.startsWith(pickedPkg)) autoPkg = `${pickedPkg}.clone`;
  });

  async function pickApk() {
    const f = await open({
      multiple: false,
      filters: [{ name: 'APK / APKS / XAPK', extensions: ['apk', 'apks', 'xapk', 'apkm'] }]
    });
    if (typeof f === 'string') filePath = f;
  }

  async function pickApkFolder() {
    const f = await open({ directory: true, multiple: false });
    if (typeof f === 'string') filePath = f;
  }

  const source = $derived<OpenSource | null>(
    srcMode === 'device'
      ? dev && ready && pickedPkg
        ? { kind: 'device', serial: dev.serial, package: pickedPkg }
        : null
      : filePath
        ? { kind: 'file', path: filePath, serial: ready && dev ? dev.serial : null }
        : null
  );

  function decompile() {
    if (!source) return;
    job.onWorkspace = (id) => {
      pendingSelect = id;
    };
    job.onEnd = async () => {
      await refreshTools();
      await refreshWorkspaces();
      if (pendingSelect && !job.error) await selectWorkspace(pendingSelect);
      pendingSelect = null;
    };
    const src = source;
    job.start(['tools', 'pull', 'decode'], (on) => api.devtoolsOpen(src, on));
  }
  let pendingSelect: string | null = null;

  const autoPkgValid = $derived(!autoClone || (PKG_RE.test(autoPkg) && autoPkg !== pickedPkg));

  async function runAuto() {
    if (!dev || !ready || !pickedPkg || !autoPkgValid) return;
    if (autoUninstall && !autoClone) {
      const ok = await confirmDialog({
        title: $t('dt.uninstallTitle'),
        message: $t('dt.uninstallMsg'),
        items: [pickedPkg],
        danger: true
      });
      if (!ok) return;
    }
    const opts: BuildOptions = {
      patch: { debuggable: autoDebuggable, newPackage: autoClone ? autoPkg : null, newLabel: null },
      install: true,
      serial: dev.serial,
      uninstallFirst: autoUninstall && !autoClone,
      exportDir: null
    };
    const serial = dev.serial;
    const pkg = pickedPkg;
    job.onWorkspace = (id) => {
      pendingSelect = id;
    };
    job.onEnd = async () => {
      await refreshTools();
      await refreshWorkspaces();
      if (pendingSelect) await selectWorkspace(pendingSelect);
      pendingSelect = null;
    };
    job.start(['tools', 'pull', 'decode', 'patch', 'build', 'sign', 'install'], (on) =>
      api.devtoolsAuto(serial, pkg, opts, on)
    );
  }

  // ------------------------------------------------------------------ //
  // Przestrzenie robocze
  // ------------------------------------------------------------------ //
  let workspaces = $state<WorkspaceMeta[]>([]);
  let ws = $state<DevWorkspace | null>(null);

  async function refreshWorkspaces() {
    try {
      workspaces = await api.devtoolsWorkspaces();
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  async function selectWorkspace(id: string) {
    try {
      ws = await api.devtoolsWorkspace(id);
      const p = ws.meta.patch;
      debuggable = p.debuggable || !ws.manifest.debuggable;
      cloneOn = !!p.newPackage;
      newPackage = p.newPackage ?? `${ws.meta.package}.clone`;
      labelOn = !!p.newLabel;
      newLabel = p.newLabel ?? (ws.meta.label ?? '');
      previewChanges = null;
      tree.clear();
      expanded.clear();
      selected = null;
      preview = null;
      await loadDir('');
    } catch (e) {
      ws = null;
      pushLog('error', errText(e), 'apk');
    }
  }

  async function deleteWorkspace(m: WorkspaceMeta) {
    const ok = await confirmDialog({
      title: $t('dt.deleteTitle'),
      message: $t('dt.deleteMsg'),
      items: [`${m.package} ${m.versionName ?? ''}`],
      danger: true
    });
    if (!ok) return;
    try {
      await api.devtoolsDelete(m.id);
      if (ws?.meta.id === m.id) ws = null;
      await refreshWorkspaces();
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  // ------------------------------------------------------------------ //
  // Edycja manifestu + build
  // ------------------------------------------------------------------ //
  let debuggable = $state(true);
  let cloneOn = $state(false);
  let newPackage = $state('');
  let labelOn = $state(false);
  let newLabel = $state('');
  let installAfter = $state(true);
  let uninstallFirst = $state(false);
  let exportDir = $state<string | null>(null);
  let previewChanges = $state<{ changes: string[]; lines: string[] } | null>(null);

  const isSplit = $derived((ws?.meta.apks.length ?? 0) > 1);
  const pkgValid = $derived(!cloneOn || (PKG_RE.test(newPackage) && newPackage !== ws?.meta.package));
  const patchOpts = $derived<PatchOptions>({
    debuggable,
    newPackage: cloneOn && !isSplit ? newPackage.trim() : null,
    newLabel: labelOn && newLabel.trim() ? newLabel.trim() : null
  });

  async function showDiff() {
    if (!ws) return;
    try {
      const [orig, res] = await Promise.all([
        api.devtoolsManifest(ws.meta.id),
        api.devtoolsPreviewPatch(ws.meta.id, patchOpts)
      ]);
      const before = new Set(orig.split('\n').map((l) => l.trim()));
      const lines = res.manifest
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => l && !before.has(l))
        .map((l) => (l.length > 400 ? l.slice(0, 400) + '…' : l));
      previewChanges = { changes: res.changes, lines };
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  async function pickExport() {
    const d = await open({ directory: true, multiple: false });
    if (typeof d === 'string') exportDir = d;
  }

  async function build() {
    if (!ws || !pkgValid) return;
    const doInstall = installAfter && ready && !!dev;
    if (doInstall && uninstallFirst && !patchOpts.newPackage) {
      const ok = await confirmDialog({
        title: $t('dt.uninstallTitle'),
        message: $t('dt.uninstallMsg'),
        items: [ws.meta.package],
        danger: true
      });
      if (!ok) return;
    }
    const opts: BuildOptions = {
      patch: patchOpts,
      install: doInstall,
      serial: doInstall && dev ? dev.serial : null,
      uninstallFirst: doInstall && uninstallFirst && !patchOpts.newPackage,
      exportDir
    };
    const id = ws.meta.id;
    job.onWorkspace = null;
    job.onEnd = async () => {
      await refreshWorkspaces();
      if (ws?.meta.id === id) ws = await api.devtoolsWorkspace(id).catch(() => ws);
    };
    const steps = ['tools', 'patch', 'build', 'sign', ...(doInstall ? ['install'] : [])];
    job.start(steps, (on) => api.devtoolsBuild(id, opts, on));
  }

  // ------------------------------------------------------------------ //
  // Eksplorator zasobów
  // ------------------------------------------------------------------ //
  const tree = new SvelteMap<string, ResEntry[]>();
  const expanded = new SvelteSet<string>();
  let loadingDir = $state<string | null>(null);
  let selected = $state<string | null>(null);
  let preview = $state<ResPreview | null>(null);
  let imgDims = $state<string>('');
  let resFilter = $state('');

  async function loadDir(rel: string) {
    if (!ws) return;
    loadingDir = rel;
    try {
      tree.set(rel, await api.devtoolsResList(ws.meta.id, rel));
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    } finally {
      loadingDir = null;
    }
  }

  async function toggleDir(rel: string) {
    if (expanded.has(rel)) {
      expanded.delete(rel);
      return;
    }
    expanded.add(rel);
    if (!tree.has(rel)) await loadDir(rel);
  }

  const rows = $derived.by(() => {
    const out: { e: ResEntry; depth: number }[] = [];
    const q = resFilter.trim().toLowerCase();
    const walk = (rel: string, depth: number) => {
      for (const e of tree.get(rel) ?? []) {
        if (!e.dir && q && !e.name.toLowerCase().includes(q)) continue;
        out.push({ e, depth });
        if (e.dir && expanded.has(e.rel)) walk(e.rel, depth + 1);
        if (out.length > 3000) return;
      }
    };
    walk('', 0);
    return out;
  });

  async function openFile(rel: string) {
    if (!ws) return;
    selected = rel;
    imgDims = '';
    try {
      preview = await api.devtoolsResPreview(ws.meta.id, rel);
    } catch (e) {
      preview = null;
      pushLog('error', errText(e), 'apk');
    }
  }

  function parentOf(rel: string) {
    const i = rel.lastIndexOf('/');
    return i < 0 ? '' : rel.slice(0, i);
  }

  async function afterResChange(rel: string) {
    if (!ws) return;
    await loadDir(parentOf(rel));
    ws = await api.devtoolsWorkspace(ws.meta.id);
    await openFile(rel);
  }

  async function replaceRes() {
    if (!ws || !preview) return;
    const name = basename(preview.rel);
    const ext = name.includes('.') ? name.split('.').pop()!.toLowerCase() : '';
    const exts = ext === 'jpg' || ext === 'jpeg' ? ['jpg', 'jpeg'] : ext ? [ext] : [];
    const f = await open({
      multiple: false,
      filters: exts.length && preview.kind === 'image' ? [{ name: ext.toUpperCase(), extensions: exts }] : []
    });
    if (typeof f !== 'string') return;
    const rel = preview.rel;
    try {
      const warns = await api.devtoolsResReplace(ws.meta.id, rel, f);
      pushLog('ok', `${$t('dt.replaced')}: ${rel}`, 'apk');
      for (const w of warns) pushLog('warn', w, 'apk');
      await afterResChange(rel);
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  async function restoreRes() {
    if (!ws || !preview) return;
    const rel = preview.rel;
    try {
      await api.devtoolsResRestore(ws.meta.id, rel);
      pushLog('ok', `${$t('dt.restored')}: ${rel}`, 'apk');
      await afterResChange(rel);
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  async function exportRes() {
    if (!ws || !preview) return;
    const dest = await save({ defaultPath: basename(preview.rel) });
    if (!dest) return;
    try {
      await api.devtoolsResExport(ws.meta.id, preview.rel, dest);
      pushLog('ok', `${$t('dt.exported')}: ${dest}`, 'apk');
    } catch (e) {
      pushLog('error', errText(e), 'apk');
    }
  }

  function fmtDate(sec: number) {
    return new Date(sec * 1000).toLocaleString();
  }

  onMount(() => {
    refreshTools();
    refreshWorkspaces();
  });

  const stepIcon = { pending: CircleDashed, active: LoaderCircle, done: CircleCheck, failed: CircleX };
</script>

<div class="space-y-4">
  <!-- Nagłówek + narzędzia -->
  <div class="card flex flex-col gap-4">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div class="flex items-center gap-3">
        <div class="flex h-10 w-10 items-center justify-center rounded-lg border border-gold/40 bg-gold/10 text-gold">
          <FileCode2 size={20} />
        </div>
        <div>
          <h2 class="view-title">{$t('dt.title')}</h2>
          <p class="text-xs text-neutral-500">{$t('dt.subtitle')}</p>
        </div>
      </div>
      <div class="flex flex-wrap gap-2">
        <button class="btn !py-1.5" onclick={importFramework} disabled={!tools?.ready} title={$t('dt.fwHint')}>
          <Layers size={15} />{$t('dt.importFw')}
        </button>
        <button class="btn !py-1.5" onclick={refreshTools}><RefreshCw size={15} /></button>
      </div>
    </div>

    <div class="flex flex-wrap items-center gap-2 border-t border-ink-600 pt-4 text-xs">
      {#each [
        { k: 'Java', v: tools?.java ? (tools.javaVersion ?? 'OK') : null, extra: tools?.javaBundled ? 'JRE 21 (app)' : '' },
        { k: 'apktool', v: tools?.apktool ? basename(tools.apktool) : null, extra: '' },
        { k: 'uber-apk-signer', v: tools?.signer ? basename(tools.signer) : null, extra: '' },
        { k: 'debug.keystore', v: tools?.keystore ? 'RSA 2048' : null, extra: '' }
      ] as it (it.k)}
        <span
          class="inline-flex max-w-full items-center gap-1.5 rounded-full border px-2.5 py-1 {it.v
            ? 'border-ok/40 bg-ok/10 text-ok'
            : 'border-ink-400 bg-ink-700 text-neutral-400'}"
          title={it.v ?? ''}
        >
          {#if it.v}<CircleCheck size={13} />{:else}<CircleDashed size={13} />{/if}
          <span class="font-medium">{it.k}</span>
          {#if it.v}<span class="max-w-[220px] truncate font-mono opacity-80">{it.v}</span>{/if}
          {#if it.extra}<span class="opacity-70">• {it.extra}</span>{/if}
        </span>
      {/each}
      {#if tools && !(tools.ready && tools.keystore)}
        <button class="btn btn-gold ml-auto !py-1.5" onclick={setupTools} disabled={job.running}>
          <Download size={15} />{$t('dt.setup')}
        </button>
      {/if}
    </div>
    {#if tools && !tools.java && !tools.canDownloadJava}
      <p class="flex items-center gap-2 text-xs text-warn"><TriangleAlert size={14} />{$t('dt.noJavaPlatform')}</p>
    {:else if tools && !tools.ready}
      <p class="text-xs text-neutral-500">{$t('dt.setupHint')}</p>
    {/if}
  </div>

  <!-- Źródło -->
  <div class="card flex flex-col gap-4">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <h3 class="text-sm font-semibold text-neutral-200">{$t('dt.source')}</h3>
      <div class="flex gap-1.5">
        <button class="chip {srcMode === 'device' ? 'chip-on' : ''}" onclick={() => (srcMode = 'device')}>
          <span class="inline-flex items-center gap-1.5"><Smartphone size={13} />{$t('dt.fromDevice')}</span>
        </button>
        <button class="chip {srcMode === 'file' ? 'chip-on' : ''}" onclick={() => (srcMode = 'file')}>
          <span class="inline-flex items-center gap-1.5"><FileArchive size={13} />{$t('dt.fromFile')}</span>
        </button>
      </div>
    </div>

    {#if srcMode === 'device'}
      {#if !ready}
        <p class="text-sm text-neutral-500">{$t('dt.needAdb')}</p>
      {:else}
        <div class="flex flex-wrap items-center gap-3">
          <div class="relative min-w-[220px] flex-1">
            <Search size={14} class="absolute left-3 top-1/2 -translate-y-1/2 text-neutral-500" />
            <input class="input !pl-9" placeholder={$t('dt.filterApps')} bind:value={appFilter} />
          </div>
          <label class="toggle whitespace-nowrap"><input type="checkbox" bind:checked={showSystem} /><span></span>{$t('dt.showSystem')}</label>
          <button class="btn !py-1.5" onclick={loadApps} disabled={appsLoading}>
            <RefreshCw size={14} class={appsLoading ? 'animate-spin' : ''} />
          </button>
        </div>
        <div class="max-h-56 overflow-y-auto rounded-lg border border-ink-600 bg-ink-900">
          {#if appsLoading && apps.length === 0}
            <div class="p-3 text-sm text-neutral-500">{$t('dt.loadingApps')}</div>
          {:else if visibleApps.length === 0}
            <div class="p-3 text-sm text-neutral-500">{$t('dt.noApps')}</div>
          {/if}
          {#each visibleApps as a (a.package)}
            <button
              class="flex w-full items-center gap-2 border-b border-ink-700 px-3 py-1.5 text-left font-mono text-xs last:border-0 hover:bg-ink-700 {pickedPkg ===
              a.package
                ? 'bg-gold/10 text-gold'
                : 'text-neutral-300'}"
              onclick={() => (pickedPkg = a.package)}
            >
              <span class="min-w-0 flex-1 truncate">{a.package}</span>
              {#if a.system}<span class="badge bg-ink-600 text-neutral-400">system</span>{/if}
              {#if !a.enabled}<span class="badge bg-warn/15 text-warn">off</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
    {:else}
      <div class="flex flex-wrap items-center gap-2">
        <button class="btn" onclick={pickApk}><FileArchive size={15} />{$t('dt.pickApk')}</button>
        <button class="btn" onclick={pickApkFolder}><FolderOpen size={15} />{$t('dt.pickFolder')}</button>
        {#if filePath}
          <span class="min-w-0 flex-1 truncate font-mono text-xs text-gold" title={filePath}>{filePath}</span>
        {/if}
      </div>
      <p class="text-xs text-neutral-500">{$t('dt.fileHint')}</p>
    {/if}

    <div class="flex flex-col gap-3 border-t border-ink-600 pt-4">
      <div class="flex flex-wrap items-center gap-2">
        <button class="btn btn-gold" onclick={decompile} disabled={!source || job.running}>
          <Play size={15} />{$t('dt.decompile')}
        </button>
        <span class="text-xs text-neutral-500">{$t('dt.decompileHint')}</span>
      </div>

      {#if srcMode === 'device' && ready}
        <div class="rounded-lg border border-gold/25 bg-gold/5 p-3">
          <div class="mb-2 flex items-center gap-2 text-sm font-medium text-gold"><Zap size={15} />{$t('dt.auto')}</div>
          <p class="mb-3 text-xs text-neutral-400">{$t('dt.autoHint')}</p>
          <div class="flex flex-row flex-wrap items-center gap-x-6 gap-y-3">
            <label class="toggle whitespace-nowrap"><input type="checkbox" bind:checked={autoDebuggable} /><span></span>{$t('dt.debuggable')}</label>
            <label class="toggle whitespace-nowrap"><input type="checkbox" bind:checked={autoClone} /><span></span>{$t('dt.clone')}</label>
            {#if autoClone}
              <input
                class="input !w-72 font-mono !text-xs {autoPkgValid ? '' : '!border-err'}"
                bind:value={autoPkg}
                spellcheck="false"
                placeholder="com.example.app.clone"
              />
            {:else}
              <label class="toggle whitespace-nowrap"><input type="checkbox" bind:checked={autoUninstall} /><span></span><span class="text-err">{$t('dt.uninstallFirst')}</span></label>
            {/if}
          </div>
          <button
            class="btn btn-gold mt-3 w-full justify-center"
            onclick={runAuto}
            disabled={!pickedPkg || job.running || !autoPkgValid || (!autoDebuggable && !autoClone)}
          >
            <Zap size={15} />{$t('dt.autoRun')}{pickedPkg ? `: ${pickedPkg}` : ''}
          </button>
        </div>
      {/if}
    </div>
  </div>

  <!-- Postęp łańcucha -->
  {#if job.running || job.ended}
    <div class="card flex flex-col gap-3">
      <div class="flex flex-wrap items-center gap-2">
        {#each job.steps as s, i (s)}
          {@const st = job.states[s]}
          {@const Icon = stepIcon[st]}
          {#if i > 0}<ChevronRight size={14} class="text-neutral-600" />{/if}
          <span
            class="inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs {st === 'done'
              ? 'border-ok/40 text-ok'
              : st === 'active'
                ? 'border-gold bg-gold/10 text-gold'
                : st === 'failed'
                  ? 'border-err bg-err/10 text-err'
                  : 'border-ink-500 text-neutral-500'}"
          >
            <Icon size={13} class={st === 'active' ? 'animate-spin' : ''} />{$t('dt.step.' + s)}
          </span>
        {/each}
        <span class="ml-auto font-mono text-lg font-semibold text-gold">{(job.permille / 10).toFixed(job.permille < 100 ? 1 : 0)}%</span>
      </div>
      <div class="relative h-2.5 overflow-hidden rounded-full bg-ink-600">
        <div
          class="h-full rounded-full bg-gradient-to-r transition-[width] duration-300 ease-out {job.error
            ? 'from-err/60 to-err'
            : 'from-gold-dim via-gold to-gold-soft'}"
          style="width: {job.permille / 10}%"
        ></div>
        {#if job.running}<div class="shimmer pointer-events-none absolute inset-0"></div>{/if}
      </div>
      {#if job.dl}
        <div class="flex items-center gap-3 text-xs text-neutral-400">
          <Download size={13} />
          <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-ink-600">
            <div class="h-full bg-gold/70 transition-[width] duration-200" style="width: {(job.dl.done / job.dl.total) * 100}%"></div>
          </div>
          <span class="font-mono">{humanBytes(job.dl.done)} / {humanBytes(job.dl.total)}</span>
        </div>
      {/if}
      {#if job.running}
        <div class="flex items-center justify-between gap-3">
          <span class="truncate font-mono text-xs text-neutral-500">{job.detail}</span>
          <button class="btn btn-danger !py-1.5" onclick={() => job.cancel()}><Square size={14} />{$t('common.cancel')}</button>
        </div>
      {:else if job.cancelled}
        <p class="text-sm text-warn">{$t('job.cancelled')}</p>
      {:else if job.error}
        <div class="rounded-lg border border-err/40 bg-err/10 p-3 text-sm">
          <div class="mb-1 flex items-center gap-2 font-medium text-err">
            <CircleX size={16} />{$t('dt.failedAt')}: {$t('dt.step.' + (job.failedStep ?? 'tools'))}
          </div>
          <p class="select-text whitespace-pre-wrap text-neutral-200">{job.error}</p>
          <p class="mt-2 text-xs text-neutral-500">{$t('dt.seeLog')}</p>
        </div>
      {:else}
        <div class="flex flex-wrap items-center gap-2 text-sm text-ok">
          <CircleCheck size={16} />{$t('job.done')}
          {#if job.result}
            <span class="text-neutral-400">— {job.result.package}{job.result.installed ? ` • ${$t('dt.installedOk')}` : ''}</span>
            <button class="btn ml-auto !py-1" onclick={() => api.revealPath((job.result?.exported[0] ?? job.result?.files[0])!)}>
              <ExternalLink size={14} />{$t('dt.showFile')}
            </button>
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  <!-- Przestrzenie robocze -->
  {#if workspaces.length}
    <div class="card flex flex-col gap-2">
      <h3 class="text-sm font-semibold text-neutral-200">{$t('dt.workspaces')}</h3>
      <div class="flex flex-col divide-y divide-ink-600">
        {#each workspaces as m (m.id)}
          <div class="flex items-center gap-3 py-2 text-sm {ws?.meta.id === m.id ? 'text-gold' : 'text-neutral-300'}">
            <button class="flex min-w-0 flex-1 items-center gap-3 text-left" onclick={() => selectWorkspace(m.id)}>
              <span class="truncate font-mono text-xs">{m.package}</span>
              {#if m.label}<span class="truncate text-xs text-neutral-500">{m.label}</span>{/if}
              {#if m.versionName}<span class="badge bg-ink-600 text-neutral-400">{m.versionName}</span>{/if}
              {#if m.apks.length > 1}<span class="badge bg-ink-600 text-neutral-400">split ×{m.apks.length}</span>{/if}
              {#if m.lastBuild}<span class="badge bg-ok/15 text-ok">build</span>{/if}
              <span class="ml-auto shrink-0 text-xs text-neutral-600">{fmtDate(m.created)}</span>
            </button>
            <button class="icon-btn" title={$t('common.delete', 'Usuń')} onclick={() => deleteWorkspace(m)} disabled={job.running}>
              <Trash2 size={15} />
            </button>
          </div>
        {/each}
      </div>
    </div>
  {/if}

  {#if ws}
    <div class="grid gap-4 2xl:grid-cols-2">
      <!-- Manifest / edycja -->
      <div class="card flex flex-col gap-4">
        <div class="flex items-center justify-between gap-3">
          <h3 class="flex items-center gap-2 text-sm font-semibold text-neutral-200"><Wrench size={15} />{$t('dt.manifest')}</h3>
          <span class="truncate font-mono text-xs text-gold">{ws.meta.package}</span>
        </div>
        <div class="grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs sm:grid-cols-3">
          <div class="text-neutral-500">{$t('dt.version')}: <span class="font-mono text-neutral-200">{ws.manifest.versionName ?? '—'} ({ws.manifest.versionCode ?? '—'})</span></div>
          <div class="text-neutral-500">SDK: <span class="font-mono text-neutral-200">{ws.manifest.minSdk ?? '?'} → {ws.manifest.targetSdk ?? '?'}</span></div>
          <div class="text-neutral-500">{$t('dt.label')}: <span class="text-neutral-200">{ws.meta.label ?? '—'}</span></div>
          <div class="text-neutral-500">{$t('dt.components')}: <span class="font-mono text-neutral-200">{ws.manifest.activities}/{ws.manifest.services}/{ws.manifest.receivers}/{ws.manifest.providers.length}</span></div>
          <div class="text-neutral-500">{$t('dt.permissions')}: <span class="font-mono text-neutral-200">{ws.manifest.usesPermissions}</span></div>
          <div class="text-neutral-500">
            debuggable:
            {#if ws.manifest.debuggable}<span class="text-ok">true</span>{:else}<span class="text-neutral-300">false</span>{/if}
          </div>
          <div class="col-span-full text-neutral-500">APK: <span class="font-mono text-neutral-300">{ws.meta.apks.join(', ')}</span> • {humanBytes(ws.meta.size)}</div>
        </div>

        <div class="flex flex-col gap-3 border-t border-ink-600 pt-4">
          <label class="toggle"><input type="checkbox" bind:checked={debuggable} /><span></span><Bug size={14} class="text-neutral-500" />{$t('dt.debuggable')}</label>
          <div class="flex flex-col gap-2">
            <label class="toggle" title={isSplit ? $t('dt.cloneSplit') : ''}>
              <input type="checkbox" bind:checked={cloneOn} disabled={isSplit} /><span></span><Copy size={14} class="text-neutral-500" />{$t('dt.clone')}
            </label>
            {#if isSplit}
              <p class="text-xs text-neutral-500">{$t('dt.cloneSplit')}</p>
            {:else if cloneOn}
              <input class="input font-mono !text-xs {pkgValid ? '' : '!border-err'}" bind:value={newPackage} spellcheck="false" />
              <p class="text-xs {pkgValid ? 'text-neutral-500' : 'text-err'}">{pkgValid ? $t('dt.cloneHint') : $t('dt.badPkg')}</p>
            {/if}
          </div>
          <div class="flex flex-col gap-2">
            <label class="toggle"><input type="checkbox" bind:checked={labelOn} /><span></span><Tag size={14} class="text-neutral-500" />{$t('dt.newLabel')}</label>
            {#if labelOn}<input class="input !text-sm" bind:value={newLabel} maxlength="60" />{/if}
          </div>
          <div>
            <button class="btn !py-1.5" onclick={showDiff}><Eye size={14} />{$t('dt.previewDiff')}</button>
          </div>
          {#if previewChanges}
            <div class="rounded-lg border border-ink-600 bg-ink-900 p-3 text-xs">
              {#if previewChanges.changes.length === 0}
                <p class="text-neutral-500">{$t('dt.noChanges')}</p>
              {:else}
                <ul class="mb-2 list-inside list-disc space-y-0.5 text-neutral-300">
                  {#each previewChanges.changes as c, i (i)}<li>{c}</li>{/each}
                </ul>
                <div class="max-h-48 overflow-auto rounded border border-ink-700 bg-black/40 p-2 font-mono text-[11px]">
                  {#each previewChanges.lines as l, i (i)}<div class="select-text whitespace-pre text-ok">+ {l}</div>{/each}
                </div>
              {/if}
            </div>
          {/if}
        </div>

        <div class="flex flex-col gap-3 border-t border-ink-600 pt-4">
          <div class="flex flex-row flex-wrap items-center gap-x-6 gap-y-3">
            <label class="toggle whitespace-nowrap"><input type="checkbox" bind:checked={installAfter} disabled={!ready} /><span></span>{$t('dt.installAfter')}</label>
            {#if installAfter && ready && !patchOpts.newPackage}
              <label class="toggle whitespace-nowrap"><input type="checkbox" bind:checked={uninstallFirst} /><span></span><span class="text-err">{$t('dt.uninstallFirst')}</span></label>
            {/if}
          </div>
          {#if !ready}<p class="text-xs text-neutral-500">{$t('dt.installNeedsAdb')}</p>{/if}
          <div class="flex flex-wrap items-center gap-2">
            <button class="btn !py-1.5" onclick={pickExport}><FolderOpen size={14} />{$t('dt.exportDir')}</button>
            {#if exportDir}
              <span class="min-w-0 flex-1 truncate font-mono text-xs text-gold" title={exportDir}>{exportDir}</span>
              <button class="icon-btn" onclick={() => (exportDir = null)}><CircleX size={14} /></button>
            {:else}
              <span class="text-xs text-neutral-500">{$t('dt.exportNone')}</span>
            {/if}
          </div>
        </div>

        <div class="mt-1 flex flex-col gap-2 border-t border-ink-600 pt-5">
          <button class="btn btn-gold w-full justify-center" onclick={build} disabled={job.running || !pkgValid}>
            <Hammer size={15} />{installAfter && ready ? $t('dt.buildInstall') : $t('dt.build')}
          </button>
          <p class="flex items-center gap-1.5 text-xs text-neutral-500"><ShieldCheck size={13} />{$t('dt.signNote')}</p>
          {#if ws.meta.lastBuild}
            <div class="flex items-center gap-2 text-xs text-neutral-400">
              <CircleCheck size={13} class="text-ok" />{$t('dt.lastBuild')}: <span class="font-mono">{ws.meta.lastBuild.package}</span>
              <span class="text-neutral-600">{fmtDate(ws.meta.lastBuild.at)}</span>
              <button class="btn ml-auto !px-2 !py-1" onclick={() => api.revealPath(ws!.meta.lastBuild!.files[0])}><ExternalLink size={13} /></button>
            </div>
          {/if}
        </div>
      </div>

      <!-- Zasoby -->
      <div class="card flex min-h-[480px] flex-col gap-3">
        <div class="flex items-center justify-between gap-3">
          <h3 class="flex items-center gap-2 text-sm font-semibold text-neutral-200"><FolderOpen size={15} />{$t('dt.resources')}</h3>
          {#if ws.meta.replaced.length}<span class="badge bg-gold/15 text-gold">{$t('dt.replacedCount')}: {ws.meta.replaced.length}</span>{/if}
        </div>
        <div class="relative">
          <Search size={14} class="absolute left-3 top-1/2 -translate-y-1/2 text-neutral-500" />
          <input class="input !py-1.5 !pl-9 !text-xs" placeholder={$t('dt.filterFiles')} bind:value={resFilter} />
        </div>
        <div class="grid min-h-0 flex-1 gap-3 sm:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
          <div class="max-h-[520px] min-h-[240px] overflow-auto rounded-lg border border-ink-600 bg-ink-900 py-1 text-xs">
            {#each rows as r (r.e.rel)}
              <button
                class="flex w-full items-center gap-1.5 py-0.5 pr-2 text-left hover:bg-ink-700 {selected === r.e.rel ? 'bg-gold/10 text-gold' : 'text-neutral-300'}"
                style="padding-left: {8 + r.depth * 14}px"
                onclick={() => (r.e.dir ? toggleDir(r.e.rel) : openFile(r.e.rel))}
              >
                {#if r.e.dir}
                  {#if loadingDir === r.e.rel}<LoaderCircle size={13} class="animate-spin text-neutral-500" />
                  {:else if expanded.has(r.e.rel)}<ChevronDown size={13} class="text-neutral-500" />
                  {:else}<ChevronRight size={13} class="text-neutral-500" />{/if}
                  <Folder size={13} class="text-gold/70" />
                {:else}
                  <span class="w-[13px]"></span>
                  {#if r.e.kind === 'image'}<FileImage size={13} class="text-sky-400/80" />
                  {:else if r.e.kind === 'xml' || r.e.kind === 'text'}<FileText size={13} class="text-neutral-500" />
                  {:else}<File size={13} class="text-neutral-600" />{/if}
                {/if}
                <span class="min-w-0 flex-1 truncate font-mono">{r.e.name}</span>
                {#if r.e.replaced}<span class="h-1.5 w-1.5 shrink-0 rounded-full bg-gold" title={$t('dt.replaced')}></span>{/if}
                <span class="shrink-0 text-[10px] text-neutral-600">{r.e.dir ? r.e.children : humanBytes(r.e.size)}</span>
              </button>
            {/each}
          </div>

          <div class="flex min-h-[240px] flex-col gap-2 rounded-lg border border-ink-600 bg-ink-900 p-3">
            {#if !preview}
              <p class="m-auto text-center text-xs text-neutral-600">{$t('dt.pickFile')}</p>
            {:else}
              <div class="truncate font-mono text-xs text-gold" title={preview.rel}>{preview.rel}</div>
              <div class="flex flex-wrap gap-2 text-[11px] text-neutral-500">
                <span>{humanBytes(preview.size)}</span>
                {#if imgDims}<span>• {imgDims}</span>{/if}
                {#if preview.ninePatch}<span class="badge bg-warn/15 text-warn">9-patch</span>{/if}
                {#if preview.replaced}<span class="badge bg-gold/15 text-gold">{$t('dt.replaced')}</span>{/if}
              </div>
              <div class="flex min-h-[140px] flex-1 items-center justify-center overflow-auto rounded border border-ink-700 checker">
                {#if preview.data && preview.mime}
                  <img
                    src={`data:${preview.mime};base64,${preview.data}`}
                    alt={basename(preview.rel)}
                    class="max-h-72 max-w-full object-contain [image-rendering:pixelated]"
                    onload={(e) => {
                      const i = e.currentTarget as HTMLImageElement;
                      imgDims = `${i.naturalWidth}×${i.naturalHeight}`;
                    }}
                  />
                {:else if preview.text != null}
                  <pre class="h-full max-h-72 w-full select-text overflow-auto whitespace-pre p-2 font-mono text-[11px] text-neutral-300">{preview.text}{preview.truncated ? '\n…' : ''}</pre>
                {:else}
                  <span class="text-xs text-neutral-600">{$t('dt.noPreview')}</span>
                {/if}
              </div>
              <div class="flex flex-wrap gap-2">
                {#if preview.replaceable}
                  <button class="btn !py-1.5" onclick={replaceRes} disabled={job.running}><Replace size={14} />{$t('dt.replace')}</button>
                {/if}
                {#if preview.replaced}
                  <button class="btn !py-1.5" onclick={restoreRes} disabled={job.running}><Undo2 size={14} />{$t('dt.restore')}</button>
                {/if}
                <button class="btn !py-1.5" onclick={exportRes}><Download size={14} />{$t('dt.export')}</button>
              </div>
              {#if preview.ninePatch}<p class="text-[11px] text-warn">{$t('dt.ninePatchNote')}</p>{/if}
            {/if}
          </div>
        </div>
        <p class="text-[11px] text-neutral-600">{$t('dt.resNote')}</p>
      </div>
    </div>
  {/if}

  <p class="text-center text-[11px] text-neutral-600">{$t('dt.legal')}</p>
</div>

<style>
  .checker {
    background-color: #151515;
    background-image:
      linear-gradient(45deg, #1f1f1f 25%, transparent 25%),
      linear-gradient(-45deg, #1f1f1f 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, #1f1f1f 75%),
      linear-gradient(-45deg, transparent 75%, #1f1f1f 75%);
    background-size: 16px 16px;
    background-position: 0 0, 0 8px, 8px -8px, -8px 0;
  }
</style>
