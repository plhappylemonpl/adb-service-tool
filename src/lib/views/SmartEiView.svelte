<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import {
    ArrowLeftRight, CloudDownload, CloudUpload, Usb, Wifi, HardDrive, FolderOpen, Play, Square, Smartphone,
    LoaderCircle, CircleCheck, CircleX, CircleDashed, Image, Camera, Film, Music, Download, FileText, MessageCircle,
    Contact, MessageSquare, Wallpaper, Settings2, Package, QrCode, Search, ShieldCheck, Info, TriangleAlert,
    PackageOpen, ExternalLink, SkipForward, ChevronRight, RotateCcw, Store, FileBox, Monitor
  } from 'lucide-svelte';
  import {
    api, type SeiConnection, type SeiModules, type SeiRole, type SeiSessionInfo, type SeiNetIf, type SeiManifest,
    type SeiDevice, type SeiImportApp, type SeiQueue, type SeiModuleEntry
  } from '$lib/api';
  import { selectedDevice, Job } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { humanBytes, errText } from '$lib/format';
  import { toast } from '$lib/components/Toast.svelte';
  import ProgressCard from '$lib/components/ProgressCard.svelte';

  const dev = $derived(selectedDevice());
  const adbReady = $derived(!!dev && dev.mode === 'adb');

  let role = $state<SeiRole>('export');
  let conn = $state<SeiConnection>('adb');

  // --- zapamiętane wybory (wygoda)
  const K = { dest: 'adbst.sei.dest', src: 'adbst.sei.src', conn: 'adbst.sei.conn' };
  function rk(k: string): string {
    try {
      return localStorage.getItem(k) ?? '';
    } catch {
      return '';
    }
  }
  function wk(k: string, v: string) {
    try {
      if (v) localStorage.setItem(k, v);
    } catch {
      /* tylko wygoda */
    }
  }
  let dest = $state(rk(K.dest));
  let source = $state(rk(K.src));
  conn = rk(K.conn) === 'wifi' ? 'wifi' : 'adb';
  $effect(() => wk(K.dest, dest));
  $effect(() => wk(K.src, source));
  $effect(() => wk(K.conn, conn));

  const CATS: { key: string; icon: typeof Camera }[] = [
    { key: 'dcim', icon: Camera },
    { key: 'pictures', icon: Image },
    { key: 'movies', icon: Film },
    { key: 'music', icon: Music },
    { key: 'download', icon: Download },
    { key: 'documents', icon: FileText },
    { key: 'whatsapp', icon: MessageCircle }
  ];

  let modules = $state<SeiModules>({
    files: ['dcim', 'pictures', 'movies', 'music', 'documents', 'whatsapp'],
    contacts: true,
    sms: true,
    wallpaper: true,
    settings: true,
    apps: true,
    apks: true,
    homescreen: true
  });

  function toggleCat(k: string) {
    modules.files = modules.files.includes(k) ? modules.files.filter((x) => x !== k) : [...modules.files, k];
  }

  // --- Wi-Fi: interfejsy
  let ifaces = $state<SeiNetIf[]>([]);
  let bindIp = $state('');
  async function loadIfaces() {
    try {
      ifaces = await api.smarteiInterfaces();
      if (!ifaces.some((i) => i.ip === bindIp)) bindIp = (ifaces.find((i) => i.recommended) ?? ifaces.find((i) => !i.virtualIf))?.ip ?? '';
    } catch (e) {
      ifaces = [];
    }
  }
  $effect(() => {
    if (conn === 'wifi') loadIfaces();
  });

  // --- import: podgląd kopii
  let srcManifest = $state<SeiManifest | null>(null);
  let srcApps = $state(0);
  let srcError = $state<string | null>(null);
  $effect(() => {
    const dir = source.trim();
    srcManifest = null;
    srcError = null;
    if (!dir || role !== 'import') return;
    const timer = setTimeout(async () => {
      try {
        const r = await api.smarteiInspect(dir);
        if (dir === source.trim()) {
          srcManifest = r.manifest;
          srcApps = r.apps;
        }
      } catch (e) {
        if (dir === source.trim()) srcError = errText(e);
      }
    }, 250);
    return () => clearTimeout(timer);
  });

  // --- przebieg
  type Step = 'setup' | 'connect' | 'review' | 'transfer' | 'done';
  const STEPS: Step[] = ['setup', 'connect', 'review', 'transfer', 'done'];
  let step = $state<Step>('setup');
  let session = $state<SeiSessionInfo | null>(null);
  let stage = $state('');
  let paired = $state(false);
  let hello = $state<{ device: SeiDevice; freeBytes: number; sizes: Record<string, number> } | null>(null);
  let review = $state<any>(null);
  let queue = $state<SeiQueue | null>(null);
  let result = $state<any>(null);
  let failure = $state<string | null>(null);
  let showApkQr = $state(false);
  let proceeding = $state(false);

  // import: wybór
  let selModules = $state<SeiModules>({ ...modules });
  let selected = $state<Record<string, boolean>>({});
  let appQuery = $state('');

  // plan B: ręczna instalacja agenta (ADB)
  let manualOpen = $state(false);
  let manualReason = $state('');
  let manualChecking = $state(false);

  const job = new Job('smartei');
  const running = $derived(job.running);

  function reset() {
    session = null;
    stage = '';
    paired = false;
    hello = null;
    review = null;
    queue = null;
    result = null;
    failure = null;
    showApkQr = false;
    proceeding = false;
    manualOpen = false;
    step = 'setup';
  }

  job.onItem = (id, ok, message) => {
    if (id === 'stage:failed') {
      failure = message;
      manualOpen = false;
    } else if (id.startsWith('stage:')) {
      stage = id.slice(6);
      if (stage === 'paired') paired = true;
      if (['export', 'import', 'files', 'apks', 'queue', 'finish', 'finalize'].includes(stage)) step = 'transfer';
      if (stage === 'cleanup') manualOpen = false;
    } else if (id === 'hello') {
      paired = true;
      try {
        hello = JSON.parse(message);
      } catch {
        /* log */
      }
    } else if (id === 'review') {
      try {
        review = JSON.parse(message);
        if (review.role === 'import') {
          const av = review.available ?? {};
          selModules = {
            files: (av.files ?? []).map((f: any) => f.key).filter((k: string) => k !== 'other'),
            contacts: !!av.contacts,
            sms: !!av.sms,
            wallpaper: !!av.wallpaper,
            settings: !!av.settings,
            apps: !!av.apps,
            apks: true,
            homescreen: false
          };
          selected = Object.fromEntries((review.apps as SeiImportApp[]).map((a) => [a.package, a.selected]));
        } else {
          selModules = { ...modules };
        }
        step = 'review';
      } catch {
        /* log */
      }
    } else if (id === 'queue') {
      try {
        queue = JSON.parse(message);
      } catch {
        /* log */
      }
    } else if (id === 'manual_install') {
      manualReason = message;
      manualChecking = false;
      manualOpen = true;
    } else if (id === 'manual_install_pending') {
      manualChecking = false;
    } else if (id === 'manual_install_done') {
      manualOpen = false;
      toast('ok', $t('backup.manual.ok'));
    } else if (id === 'result' && ok) {
      try {
        result = JSON.parse(message);
      } catch {
        /* log */
      }
    }
  };

  job.onFinished = (r) => {
    manualOpen = false;
    if (r.cancelled) {
      toast('warn', $t('sei.cancelled'));
      step = 'setup';
      return;
    }
    if (result) {
      step = 'done';
      toast(r.failures.length ? 'warn' : 'ok', $t(result.role === 'import' ? 'sei.done.import' : 'sei.done.export'));
    } else {
      toast('error', $t('sei.failed'), failure ?? r.failures[0]?.error ?? '');
      // błąd (np. agent nie połączył się) - wracamy do ustawień, "Połącz" działa ponownie
      failure = failure ?? r.failures[0]?.error ?? null;
      step = 'setup';
    }
  };

  const canConnect = $derived(
    !running &&
      (conn === 'wifi' ? ifaces.some((i) => !i.virtualIf) || !!bindIp : adbReady) &&
      (role === 'export' ? !!dest.trim() && (modules.files.length > 0 || modules.contacts || modules.sms || modules.wallpaper || modules.settings || modules.apps) : !!srcManifest)
  );

  async function connect() {
    if (!canConnect) return;
    reset();
    step = 'connect';
    const mods = $state.snapshot(modules);
    if (conn === 'wifi') mods.homescreen = false;
    await job.start(async (on) => {
      const info = await api.smarteiStart(
        {
          role,
          connection: conn,
          serial: conn === 'adb' ? dev?.serial : null,
          dir: role === 'export' ? dest.trim() : source.trim(),
          modules: mods,
          bindIp: conn === 'wifi' ? bindIp || null : null
        },
        on
      );
      session = info;
      return info.jobId;
    });
    if (!job.running) step = 'setup';
  }

  async function proceed() {
    if (!session || proceeding) return;
    proceeding = true;
    const packages = Object.entries(selected).filter(([, v]) => v).map(([k]) => k);
    try {
      await api.smarteiProceed(session.sessionId, { modules: $state.snapshot(selModules), packages });
      step = 'transfer';
    } catch (e) {
      toast('error', $t('sei.failed'), errText(e));
    } finally {
      proceeding = false;
    }
  }

  async function queueAction(a: 'next' | 'skip') {
    if (session) await api.smarteiQueue(session.sessionId, a).catch(() => {});
  }

  async function openApkFolder() {
    try {
      const p = await api.smarteiAgentApk();
      await api.revealPath(p);
    } catch (e) {
      toast('error', 'Agent.apk', errText(e));
    }
  }

  async function confirmManual() {
    if (!dev) return;
    manualChecking = true;
    try {
      if (!(await api.agentInstallConfirm(dev.serial))) manualChecking = false;
    } catch {
      manualChecking = false;
    }
  }

  async function pickDest() {
    const d = await open({ directory: true, multiple: false, defaultPath: dest || undefined });
    if (typeof d === 'string') dest = d;
  }
  async function pickSource() {
    const d = await open({ directory: true, multiple: false, defaultPath: source || dest || undefined });
    if (typeof d === 'string') source = d;
  }

  function switchRole(r: SeiRole) {
    if (running || r === role) return;
    role = r;
    reset();
  }

  // --- podsumowanie
  const reviewApps = $derived<SeiImportApp[]>(review?.apps ?? []);
  const filteredApps = $derived(
    reviewApps.filter((a) => {
      const q = appQuery.trim().toLowerCase();
      return !q || a.label.toLowerCase().includes(q) || a.package.toLowerCase().includes(q);
    })
  );
  const selCount = $derived(reviewApps.filter((a) => selected[a.package]).length);
  const importFiles = $derived<{ key: string; count: number; bytes: number }[]>(review?.available?.files ?? []);
  const importNeed = $derived(
    importFiles.filter((f) => selModules.files.includes(f.key) || (f.key === 'other' && selModules.files.length > 0)).reduce((s, f) => s + f.bytes, 0)
  );
  const phoneEnough = $derived(!review?.targetFree || importNeed + 300 * 1024 * 1024 <= review.targetFree);
  const exportEstimate = $derived(
    review?.role === 'export'
      ? Object.entries((review.sizes ?? {}) as Record<string, number>)
          .filter(([k]) => (selModules.files.includes(k)) || (k === 'apks' && selModules.apks && selModules.apps) || (k === 'contacts' && selModules.contacts) || (k === 'sms' && selModules.sms) || (k === 'wallpaper' && selModules.wallpaper))
          .reduce((s, [, v]) => s + v, 0)
      : 0
  );
  const pcEnough = $derived(!review?.free || review.free >= exportEstimate + exportEstimate / 20 + 200 * 1024 * 1024);

  function setAll(v: boolean) {
    selected = Object.fromEntries(reviewApps.map((a) => [a.package, v && a.reason !== 'noApk']));
  }

  const stepIdx = $derived(STEPS.indexOf(step));
  const stepIcon = (i: number) => (i < stepIdx || step === 'done' ? CircleCheck : i === stepIdx ? LoaderCircle : CircleDashed);
  const reasonKey = (r: SeiImportApp['reason']) =>
    r === 'sourceVendor' ? 'sei.apps.vendor' : r === 'alreadyInstalled' ? 'sei.apps.installed' : r === 'noApk' ? 'sei.apps.noApk' : '';
  const devLabel = (d: SeiDevice | null | undefined) =>
    !d ? '—' : d.model.toLowerCase().startsWith(d.manufacturer.toLowerCase()) ? d.model : `${d.manufacturer} ${d.model}`;
  const isWindows = typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent);
  const modName = (n: string) =>
    n === 'apks' ? $t('sei.mod.apkFiles') : ['files', 'contacts', 'sms', 'wallpaper', 'settings', 'apps'].includes(n) ? $t('sei.mod.' + n) : n;
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex flex-wrap items-end justify-between gap-3">
    <div class="flex items-center gap-3">
      <div class="flex h-11 w-11 items-center justify-center rounded-xl border border-gold/50 bg-gold/10 text-gold"><ArrowLeftRight size={22} /></div>
      <div>
        <h2 class="view-title">{$t('sei.title')}</h2>
        <p class="text-xs text-neutral-500">{$t('sei.subtitle')}</p>
      </div>
    </div>
    <div class="flex rounded-lg border border-ink-600 bg-ink-900 p-1 text-sm" role="tablist">
      {#each [{ r: 'export' as SeiRole, icon: CloudDownload, k: 'sei.tabExport' }, { r: 'import' as SeiRole, icon: CloudUpload, k: 'sei.tabImport' }] as tb (tb.r)}
        {@const Icon = tb.icon}
        <button
          role="tab"
          aria-selected={role === tb.r}
          disabled={running && role !== tb.r}
          class="flex items-center gap-2 rounded-md px-3 py-1.5 transition-colors disabled:opacity-40 {role === tb.r ? 'bg-gold/15 text-gold' : 'text-neutral-400 hover:text-neutral-200'}"
          onclick={() => switchRole(tb.r)}
        ><Icon size={15} />{$t(tb.k)}</button>
      {/each}
    </div>
  </div>

  <!-- kroki 1 -> 5 -->
  <ol class="card flex flex-wrap items-center gap-x-2 gap-y-2 !py-3 text-sm">
    {#each STEPS as s, i (s)}
      {@const Icon = stepIcon(i)}
      <li class="flex items-center gap-2 {i < stepIdx || step === 'done' ? 'text-ok' : i === stepIdx ? 'text-gold' : 'text-neutral-500'}">
        <Icon size={16} class={i === stepIdx && running && step !== 'done' && step !== 'setup' ? 'animate-spin' : ''} />
        <span class="font-mono text-xs opacity-60">{i + 1}.</span>{$t('sei.step.' + s)}
      </li>
      {#if i < STEPS.length - 1}<ChevronRight size={14} class="text-neutral-600" />{/if}
    {/each}
  </ol>

  {#if failure && !running}
    <div class="rounded-lg border border-err/40 bg-err/10 p-3 text-sm">
      <div class="mb-1 flex items-center gap-2 font-medium text-err"><CircleX size={16} />{$t('sei.failed')}</div>
      <p class="select-text whitespace-pre-wrap text-neutral-200">{failure}</p>
    </div>
  {/if}

  <!-- ============ 1. USTAWIENIA ============ -->
  {#if step === 'setup'}
    <div class="grid gap-4 xl:grid-cols-[1fr_360px]">
      <div class="space-y-4">
        <div class="card space-y-3">
          <span class="label">{$t('sei.conn.title')}</span>
          <div class="grid gap-3 md:grid-cols-2">
            {#each [{ c: 'adb' as SeiConnection, icon: Usb, k: 'adb' }, { c: 'wifi' as SeiConnection, icon: Wifi, k: 'wifi' }] as o (o.c)}
              {@const Icon = o.icon}
              <button
                class="flex items-start gap-3 rounded-xl border p-3 text-left transition-colors {conn === o.c ? 'border-gold/70 bg-gold/10' : 'border-ink-600 bg-ink-900 hover:border-ink-500'}"
                onclick={() => (conn = o.c)}
              >
                <Icon size={20} class="mt-0.5 shrink-0 {conn === o.c ? 'text-gold' : 'text-neutral-500'}" />
                <span>
                  <span class="block text-sm font-medium {conn === o.c ? 'text-gold' : 'text-neutral-200'}">{$t('sei.conn.' + o.k)}</span>
                  <span class="mt-0.5 block text-xs text-neutral-400">{$t('sei.conn.' + o.k + 'Desc')}</span>
                </span>
              </button>
            {/each}
          </div>
          {#if conn === 'adb' && !adbReady}
            <div class="flex items-start gap-2 rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-xs text-warn"><TriangleAlert size={14} class="mt-px shrink-0" />{$t('sei.needAdb')}</div>
          {/if}
          {#if conn === 'wifi'}
            <label class="block">
              <span class="label">{$t('sei.iface')}</span>
              <select class="input" bind:value={bindIp}>
                {#each ifaces as i (i.ip)}
                  <option value={i.ip}>{i.ip} — {i.name}{i.recommended ? ` (${$t('sei.ifaceRecommended')})` : i.virtualIf ? ` (${$t('sei.ifaceVirtual')})` : ''}</option>
                {/each}
              </select>
            </label>
            {#if !ifaces.some((i) => !i.virtualIf)}
              <div class="flex items-start gap-2 rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-xs text-warn"><TriangleAlert size={14} class="mt-px shrink-0" />{$t('sei.noLan')}</div>
            {/if}
          {/if}
        </div>

        {#if role === 'export'}
          <div class="card space-y-3">
            <span class="label">{$t('sei.what')}</span>
            <div>
              <div class="mb-2 flex items-center gap-2 text-sm text-neutral-300"><FolderOpen size={15} class="text-gold" />{$t('sei.mod.files')}</div>
              <div class="grid grid-cols-1 gap-2 sm:grid-cols-2 2xl:grid-cols-3">
                {#each CATS as c (c.key)}
                  {@const Icon = c.icon}
                  <label class="flex cursor-pointer items-center gap-2 rounded-lg border border-ink-600 bg-ink-900 px-2.5 py-2 text-sm {modules.files.includes(c.key) ? 'text-neutral-100' : 'text-neutral-500'}">
                    <input type="checkbox" class="accent-[#c9a249]" checked={modules.files.includes(c.key)} onchange={() => toggleCat(c.key)} />
                    <Icon size={14} class="shrink-0 text-gold" />{$t('sei.cat.' + c.key)}
                  </label>
                {/each}
              </div>
            </div>
            <div class="grid gap-2 border-t border-ink-600 pt-3 sm:grid-cols-2">
              {#each [{ k: 'contacts', icon: Contact }, { k: 'sms', icon: MessageSquare }, { k: 'wallpaper', icon: Wallpaper }, { k: 'settings', icon: Settings2 }, { k: 'apps', icon: Package }] as m (m.k)}
                {@const Icon = m.icon}
                <label class="flex cursor-pointer items-center gap-2 text-sm text-neutral-200">
                  <input type="checkbox" class="accent-[#c9a249]" bind:checked={(modules as any)[m.k]} />
                  <Icon size={14} class="text-gold" />{$t('sei.mod.' + m.k)}
                </label>
              {/each}
            </div>
            <div class="space-y-1.5 text-xs text-neutral-400">
              <label class="flex cursor-pointer items-start gap-2 {modules.apps ? '' : 'opacity-40'}">
                <input type="checkbox" class="mt-0.5 accent-[#c9a249]" bind:checked={modules.apks} disabled={!modules.apps} />{$t('sei.mod.apks')}
              </label>
              <label class="flex cursor-pointer items-start gap-2 {conn === 'adb' ? '' : 'opacity-40'}">
                <input type="checkbox" class="mt-0.5 accent-[#c9a249]" bind:checked={modules.homescreen} disabled={conn !== 'adb'} />{$t('sei.mod.homescreen')}
              </label>
              {#if modules.settings}<p class="flex items-start gap-1.5"><Info size={13} class="mt-px shrink-0 text-gold" />{$t('sei.mod.settingsHint')}</p>{/if}
            </div>
          </div>
        {/if}
      </div>

      <div class="space-y-4">
        <div class="card space-y-3">
          {#if role === 'export'}
            <div>
              <span class="label">{$t('sei.dest')}</span>
              <div class="flex gap-2">
                <input class="input flex-1" bind:value={dest} placeholder={$t('pull.destPh')} />
                <button class="btn !px-3" onclick={pickDest} title={$t('pull.pickDest')}><HardDrive size={15} /></button>
              </div>
              <p class="mt-1.5 text-[11px] text-neutral-500">{$t('sei.destHint')}</p>
            </div>
          {:else}
            <div>
              <span class="label">{$t('sei.source')}</span>
              <div class="flex gap-2">
                <input class="input flex-1" bind:value={source} placeholder={$t('sei.sourceHint')} />
                <button class="btn !px-3" onclick={pickSource} title={$t('restore.pick')}><FolderOpen size={15} /></button>
              </div>
            </div>
            {#if srcManifest}
              <div class="space-y-1.5 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs">
                <div class="flex items-center gap-2 text-neutral-300"><Smartphone size={13} class="text-gold" />{$t('sei.backupOf')}: <b class="text-neutral-100">{devLabel(srcManifest.source)}</b></div>
                <div class="text-neutral-500">{$t('sei.created')}: {new Date(srcManifest.created).toLocaleString()} • Android {srcManifest.source.androidVersion}</div>
                {#each srcManifest.modules as m (m.name)}
                  <div class="flex items-center gap-2 text-neutral-400">
                    {#if m.ok}<CircleCheck size={12} class="text-ok" />{:else}<CircleDashed size={12} />{/if}
                    {modName(m.name)}<span class="ml-auto font-mono">{m.items}{m.bytes ? ` • ${humanBytes(m.bytes)}` : ''}</span>
                  </div>
                {/each}
                {#if !srcManifest.complete}<div class="text-warn">{$t('sei.incomplete')}</div>{/if}
              </div>
            {:else if srcError && source.trim()}
              <div class="flex items-start gap-1.5 rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-xs text-warn"><FileBox size={13} class="mt-px shrink-0" />{$t('sei.noManifest')}</div>
            {/if}
          {/if}
          {#if conn === 'adb' && dev}
            <div class="flex items-center gap-2 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs text-neutral-400">
              <Smartphone size={14} class="shrink-0 text-gold" /><span class="truncate">{dev.modelHint ?? dev.serial}</span>
            </div>
          {/if}
        </div>
        <button class="btn btn-gold w-full justify-center py-3 text-base" disabled={!canConnect} onclick={connect}>
          <Play size={17} />{$t('sei.connect')}
        </button>
        <div class="card space-y-2 text-xs text-neutral-400">
          <div class="flex items-center gap-2 text-sm text-neutral-200"><Info size={15} class="text-gold" />{$t('sei.limitsTitle')}</div>
          {#each ['limit1', 'limit2', 'limit3', 'limit4'] as l (l)}
            <p class="flex items-start gap-1.5"><ChevronRight size={12} class="mt-0.5 shrink-0 text-gold" />{$t('sei.' + l)}</p>
          {/each}
        </div>
      </div>
    </div>
  {/if}

  <!-- ============ 2. POŁĄCZENIE ============ -->
  {#if step === 'connect'}
    {#if conn === 'adb'}
      <div class="card flex items-center gap-4">
        <div class="relative flex h-14 w-14 shrink-0 items-center justify-center rounded-xl border border-gold/50 bg-gold/10 text-gold">
          <Smartphone size={26} /><span class="absolute -right-1 -top-1 h-3 w-3 animate-ping rounded-full bg-gold"></span>
        </div>
        <div class="space-y-1 text-sm">
          <div class="text-neutral-100">{$t(stage === 'deploy' ? 'sei.adb.deploy' : stage === 'launch' ? 'sei.adb.launch' : 'sei.adb.wait')}</div>
          <div class="text-xs text-neutral-500">{dev?.modelHint ?? dev?.serial}</div>
        </div>
        <button class="btn btn-danger ml-auto !py-1.5" onclick={() => job.cancel()}><Square size={14} />{$t('common.cancel')}</button>
      </div>
    {:else if session}
      <div class="grid gap-4 lg:grid-cols-3">
        <div class="card space-y-3">
          <div class="flex items-center gap-2 text-sm font-medium text-gold"><span class="font-mono">1.</span>{$t('sei.wifi.step1')}</div>
          <p class="text-xs text-neutral-400">{$t('sei.wifi.step1Text')}</p>
          <button class="btn w-full justify-center" onclick={openApkFolder}><FolderOpen size={15} />{$t('sei.wifi.openApk')}</button>
          <button class="btn w-full justify-center" onclick={() => (showApkQr = !showApkQr)}><QrCode size={15} />{$t('sei.wifi.apkQr')}</button>
          {#if showApkQr && session.apkQr}
            <div class="mx-auto w-44 overflow-hidden rounded-lg bg-white p-1 [&_svg]:h-auto [&_svg]:w-full">{@html session.apkQr}</div>
            <div class="select-text break-all text-center font-mono text-[11px] text-neutral-500">{session.apkUrl}</div>
          {/if}
        </div>
        <div class="card space-y-3 border-gold/40">
          <div class="flex items-center gap-2 text-sm font-medium text-gold"><span class="font-mono">2.</span>{$t('sei.wifi.step2')}</div>
          <p class="text-xs text-neutral-400">{$t('sei.wifi.step2Text')}</p>
          <div class="mx-auto w-56 overflow-hidden rounded-xl bg-white p-1.5 [&_svg]:h-auto [&_svg]:w-full">{@html session.pairQr}</div>
          <div class="grid grid-cols-1 gap-2 text-center">
            <div class="rounded-lg border border-ink-600 bg-ink-900 p-2">
              <div class="text-[10px] uppercase tracking-wider text-neutral-500">{$t('sei.wifi.address')}</div>
              <div class="select-text break-all font-mono text-base text-gold">{session.ip}:{session.port}</div>
            </div>
            <div class="rounded-lg border border-ink-600 bg-ink-900 p-2">
              <div class="text-[10px] uppercase tracking-wider text-neutral-500">{$t('sei.wifi.pin')}</div>
              <div class="select-text font-mono text-xl tracking-[0.25em] text-gold">{session.pin}</div>
            </div>
          </div>
        </div>
        <div class="card space-y-3">
          <div class="flex items-center gap-2 text-sm font-medium text-gold"><span class="font-mono">3.</span>{$t('sei.wifi.step3')}</div>
          <p class="text-xs text-neutral-400">{$t('sei.wifi.step3Text')}</p>
          <div class="flex items-center gap-2 rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-sm {paired ? 'text-ok' : 'text-neutral-300'}">
            {#if paired}<CircleCheck size={16} />{$t('sei.paired')}{:else}<LoaderCircle size={16} class="animate-spin text-gold" />{$t('sei.wifi.waiting')}{/if}
          </div>
          {#if isWindows}<p class="flex items-start gap-1.5 text-[11px] text-neutral-500"><ShieldCheck size={13} class="mt-px shrink-0 text-warn" />{$t('sei.wifi.firewall')}</p>{/if}
          <button class="btn btn-danger w-full justify-center !py-1.5" onclick={() => job.cancel()}><Square size={14} />{$t('common.cancel')}</button>
        </div>
      </div>
    {:else}
      <div class="card flex items-center gap-2 text-sm text-neutral-300"><LoaderCircle size={16} class="animate-spin text-gold" />{$t('job.running')}</div>
    {/if}
  {/if}

  <!-- ============ 3. PODSUMOWANIE ============ -->
  {#if step === 'review' && review}
    <div class="grid gap-4 xl:grid-cols-[1fr_360px]">
      <div class="space-y-4">
        {#if review.role === 'export'}
          <div class="card space-y-3">
            <span class="label">{$t('sei.review.title')}</span>
            <div class="grid grid-cols-1 gap-2 sm:grid-cols-2 2xl:grid-cols-3">
              {#each CATS.filter((c) => modules.files.includes(c.key)) as c (c.key)}
                {@const Icon = c.icon}
                <label class="flex cursor-pointer items-center gap-2 rounded-lg border border-ink-600 bg-ink-900 px-2.5 py-2 text-sm">
                  <input type="checkbox" class="accent-[#c9a249]" checked={selModules.files.includes(c.key)}
                    onchange={() => (selModules.files = selModules.files.includes(c.key) ? selModules.files.filter((x) => x !== c.key) : [...selModules.files, c.key])} />
                  <Icon size={14} class="shrink-0 text-gold" />
                  <span class="min-w-0 leading-tight"><span class="block truncate">{$t('sei.cat.' + c.key)}</span>
                    <span class="block font-mono text-[11px] text-neutral-500">{humanBytes(review.sizes?.[c.key] ?? 0)}</span></span>
                </label>
              {/each}
            </div>
            <div class="grid gap-2 border-t border-ink-600 pt-3 sm:grid-cols-2">
              {#each ['contacts', 'sms', 'wallpaper', 'settings', 'apps'].filter((k) => (modules as any)[k]) as k (k)}
                <label class="flex cursor-pointer items-center gap-2 text-sm text-neutral-200">
                  <input type="checkbox" class="accent-[#c9a249]" bind:checked={(selModules as any)[k]} />{$t('sei.mod.' + k)}
                </label>
              {/each}
            </div>
          </div>
        {:else}
          <div class="card space-y-3">
            <div class="flex flex-wrap items-center gap-3 text-sm">
              <span class="flex items-center gap-2 text-neutral-300"><Smartphone size={15} class="text-neutral-500" />{$t('sei.review.from')}: <b class="text-neutral-100">{devLabel(review.source)}</b></span>
              <ArrowLeftRight size={15} class="text-gold" />
              <span class="flex items-center gap-2 text-neutral-300"><Smartphone size={15} class="text-gold" />{$t('sei.review.to')}: <b class="text-neutral-100">{devLabel(review.target)}</b></span>
            </div>
            {#if review.crossVendor}
              <div class="flex items-start gap-2 rounded-lg border border-gold/40 bg-gold/10 p-2.5 text-xs text-gold"><Info size={14} class="mt-px shrink-0" />{$t('sei.review.crossVendor')}</div>
            {/if}
            {#if review.problems?.length}
              <div class="rounded-lg border border-warn/40 bg-warn/10 p-2.5 text-xs text-warn">
                <div class="mb-1 flex items-center gap-1.5"><TriangleAlert size={13} />{$t('sei.review.problems')}</div>
                {#each review.problems.slice(0, 6) as p (p)}<div class="truncate font-mono">{p}</div>{/each}
              </div>
            {/if}
            <div class="grid grid-cols-1 gap-2 sm:grid-cols-2 2xl:grid-cols-3">
              {#each importFiles.filter((f) => f.key !== 'other') as f (f.key)}
                {@const Icon = CATS.find((c) => c.key === f.key)?.icon ?? FolderOpen}
                <label class="flex cursor-pointer items-center gap-2 rounded-lg border border-ink-600 bg-ink-900 px-2.5 py-2 text-sm">
                  <input type="checkbox" class="accent-[#c9a249]" checked={selModules.files.includes(f.key)}
                    onchange={() => (selModules.files = selModules.files.includes(f.key) ? selModules.files.filter((x) => x !== f.key) : [...selModules.files, f.key])} />
                  <Icon size={14} class="shrink-0 text-gold" />
                  <span class="min-w-0 leading-tight"><span class="block truncate">{$t('sei.cat.' + f.key)}</span>
                    <span class="block font-mono text-[11px] text-neutral-500">{f.count} • {humanBytes(f.bytes)}</span></span>
                </label>
              {/each}
            </div>
            <div class="grid gap-2 border-t border-ink-600 pt-3 sm:grid-cols-3">
              {#each [{ k: 'contacts', icon: Contact }, { k: 'sms', icon: MessageSquare }, { k: 'wallpaper', icon: Wallpaper }, { k: 'settings', icon: Settings2 }, { k: 'apps', icon: Package }] as m (m.k)}
                {@const Icon = m.icon}
                {@const avail = !!review.available?.[m.k]}
                <label class="flex items-center gap-2 text-sm {avail ? 'cursor-pointer text-neutral-200' : 'text-neutral-600'}">
                  <input type="checkbox" class="accent-[#c9a249]" bind:checked={(selModules as any)[m.k]} disabled={!avail} />
                  <Icon size={14} class={avail ? 'text-gold' : ''} />{$t('sei.mod.' + m.k)}
                </label>
              {/each}
            </div>
          </div>

          {#if reviewApps.length && selModules.apps}
            <div class="card space-y-3">
              <div class="flex flex-wrap items-center gap-2">
                <span class="label !mb-0">{$t('sei.apps.title')}</span>
                <span class="text-xs text-neutral-500">{$t('sei.apps.selected').replace('{n}', String(selCount))} / {reviewApps.length}</span>
                <span class="ml-auto flex gap-2">
                  <button class="btn !py-1 text-xs" onclick={() => setAll(true)}>{$t('sei.apps.all')}</button>
                  <button class="btn !py-1 text-xs" onclick={() => setAll(false)}>{$t('sei.apps.none')}</button>
                </span>
              </div>
              <div class="relative">
                <Search size={14} class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-neutral-500" />
                <input class="input !py-1.5 !pl-8 text-sm" placeholder={$t('sei.apps.search')} bind:value={appQuery} />
              </div>
              <div class="max-h-[420px] divide-y divide-ink-700 overflow-y-auto rounded-lg border border-ink-600">
                {#each filteredApps as a (a.package)}
                  <label class="flex items-center gap-3 px-3 py-2 text-sm {a.reason === 'noApk' ? 'opacity-50' : 'cursor-pointer hover:bg-ink-800'}">
                    <input type="checkbox" class="accent-[#c9a249]" bind:checked={selected[a.package]} disabled={a.reason === 'noApk'} />
                    <span class="min-w-0 flex-1">
                      <span class="block truncate text-neutral-100">{a.label}</span>
                      <span class="block truncate font-mono text-[11px] text-neutral-500">{a.package}{a.versionName ? ` • ${a.versionName}` : ''}</span>
                      {#if a.reason}<span class="block text-[11px] {a.reason === 'sourceVendor' ? 'text-warn' : 'text-neutral-400'}">{$t(reasonKey(a.reason))}</span>{/if}
                    </span>
                    {#if a.source === 'play_store'}
                      <span class="flex shrink-0 items-center gap-1 rounded-full border border-ok/40 bg-ok/10 px-2 py-0.5 text-[11px] text-ok"><Store size={11} />{$t('sei.apps.play')}</span>
                    {:else}
                      <span class="flex shrink-0 items-center gap-1 rounded-full border border-gold/40 bg-gold/10 px-2 py-0.5 text-[11px] text-gold"><PackageOpen size={11} />{$t('sei.apps.sideload')}</span>
                    {/if}
                  </label>
                {/each}
              </div>
            </div>
          {/if}
        {/if}
      </div>

      <div class="space-y-4">
        <div class="card space-y-2 text-sm">
          {#if review.role === 'export'}
            <div class="flex justify-between"><span class="text-neutral-400">{$t('sei.review.estimate')}</span><span class="font-mono text-gold">{humanBytes(exportEstimate)}</span></div>
            <div class="flex justify-between"><span class="text-neutral-400">{$t('sei.review.free')}</span><span class="font-mono {pcEnough ? 'text-ok' : 'text-err'}">{review.free ? humanBytes(review.free) : '—'}</span></div>
            {#if !pcEnough}<div class="text-xs text-err">{$t('sei.review.notEnough')}</div>{/if}
            {#if review.resumed}<div class="text-xs text-gold">{$t('sei.resume')}</div>{/if}
          {:else}
            <div class="flex justify-between"><span class="text-neutral-400">{$t('sei.review.need')}</span><span class="font-mono text-gold">{humanBytes(importNeed)}</span></div>
            <div class="flex justify-between"><span class="text-neutral-400">{$t('sei.review.phoneFree')}</span><span class="font-mono {phoneEnough ? 'text-ok' : 'text-err'}">{review.targetFree ? humanBytes(review.targetFree) : '—'}</span></div>
            {#if !phoneEnough}<div class="text-xs text-err">{$t('sei.review.phoneNotEnough')}</div>{/if}
          {/if}
        </div>
        <button class="btn btn-gold w-full justify-center py-3 text-base" disabled={proceeding || (review.role === 'export' ? !pcEnough : !phoneEnough)} onclick={proceed}>
          <Play size={17} />{$t('sei.start')}
        </button>
        <p class="flex items-start gap-1.5 text-xs text-neutral-400"><Smartphone size={13} class="mt-px shrink-0 text-gold" />{$t(review.role === 'export' ? 'sei.review.phoneExport' : 'sei.review.phoneImport')}</p>
        <button class="btn btn-danger w-full justify-center !py-1.5" onclick={() => job.cancel()}><Square size={14} />{$t('common.cancel')}</button>
      </div>
    </div>
  {/if}

  <!-- ============ 4. PRZENOSZENIE ============ -->
  {#if step === 'transfer'}
    <div class="grid gap-4 xl:grid-cols-[1fr_360px]">
      <div class="space-y-4">
        <div class="card flex items-center gap-3 text-sm">
          <LoaderCircle size={18} class="animate-spin text-gold" />
          <span class="text-neutral-100">{$t('sei.stage.' + (['export', 'import', 'files', 'apks', 'queue', 'finish', 'finalize', 'cleanup'].includes(stage) ? stage : role))}</span>
          {#if hello}<span class="ml-auto text-xs text-neutral-500">{devLabel(hello.device)}</span>{/if}
        </div>
        <ProgressCard {job} />
        {#if queue?.active && !queue.finished}
          <div class="card space-y-3 border-gold/40">
            <div class="flex items-center gap-2">
              <Store size={18} class="text-gold" />
              <span class="text-sm font-medium text-neutral-100">{$t('sei.queue.title')}</span>
              <span class="ml-auto font-mono text-lg text-gold">{queue.position} / {queue.total}</span>
            </div>
            <div class="rounded-lg border border-ink-600 bg-ink-900 p-3">
              <div class="text-base text-neutral-100">{queue.current?.label}</div>
              <div class="font-mono text-[11px] text-neutral-500">{queue.current?.package}</div>
            </div>
            <p class="text-xs text-neutral-400">{$t('sei.queue.hint')}</p>
            <div class="flex gap-2">
              <button class="btn btn-gold flex-1 justify-center" onclick={() => queueAction('next')}><ChevronRight size={15} />{$t('sei.queue.next')}</button>
              <button class="btn flex-1 justify-center" onclick={() => queueAction('skip')}><SkipForward size={15} />{$t('sei.queue.skip')}</button>
            </div>
          </div>
        {/if}
      </div>
      <div class="space-y-4">
        <div class="card space-y-2 text-xs text-neutral-400">
          <div class="flex items-center gap-2 text-sm text-neutral-200"><Monitor size={15} class="text-gold" />{session?.connection === 'wifi' ? `Wi-Fi • ${session.ip}:${session.port}` : 'ADB (USB)'}</div>
          <p>{$t(role === 'export' ? 'sei.review.phoneExport' : 'sei.review.phoneImport')}</p>
        </div>
      </div>
    </div>
  {/if}

  <!-- ============ 5. GOTOWE ============ -->
  {#if step === 'done' && result}
    <div class="card space-y-4 border-ok/40">
      <div class="flex items-center gap-2 text-base font-medium text-ok"><CircleCheck size={20} />{$t(result.role === 'import' ? 'sei.done.import' : 'sei.done.export')}</div>
      {#if result.dir}<div class="select-text truncate font-mono text-xs text-neutral-400" title={result.dir}>{result.dir}</div>{/if}
      <div class="grid grid-cols-1 gap-2 sm:grid-cols-2 2xl:grid-cols-3">
        {#each (result.modules ?? []) as m (m.name)}
          {@const mm = m as SeiModuleEntry}
          <div class="rounded-lg border border-ink-600 bg-ink-900 p-3 text-sm">
            <div class="flex items-center gap-2 {mm.ok ? 'text-ok' : 'text-warn'}">{#if mm.ok}<CircleCheck size={14} />{:else}<TriangleAlert size={14} />{/if}{modName(mm.name)}</div>
            <div class="mt-1 font-mono text-xl text-gold">{mm.items}</div>
            {#if mm.bytes}<div class="text-xs text-neutral-500">{humanBytes(mm.bytes)}</div>{/if}
            {#if mm.note}<div class="truncate text-[11px] text-neutral-500" title={mm.note}>{mm.note}</div>{/if}
          </div>
        {/each}
        {#if result.queue?.total}
          <div class="rounded-lg border border-ink-600 bg-ink-900 p-3 text-sm">
            <div class="flex items-center gap-2 text-ok"><Store size={14} />Google Play</div>
            <div class="mt-1 font-mono text-xl text-gold">{result.queue.installed} / {result.queue.total}</div>
          </div>
        {/if}
      </div>
      {#if result.warnings?.length}
        <div class="rounded-lg border border-warn/40 bg-warn/10 p-3 text-xs text-warn">
          <div class="mb-1 flex items-center gap-1.5"><TriangleAlert size={13} />{$t('sei.done.warnings')}</div>
          {#each result.warnings.slice(0, 12) as w (w)}<div class="select-text">{w}</div>{/each}
        </div>
      {/if}
      <div class="flex flex-wrap gap-2">
        {#if result.dir}<button class="btn !py-1.5" onclick={() => api.revealPath(result.dir + '/manifest.json')}><ExternalLink size={14} />{$t('sei.done.openFolder')}</button>{/if}
        {#if result.role === 'export'}
          <button class="btn !py-1.5" onclick={() => { source = result.dir; role = 'import'; reset(); }}><CloudUpload size={14} />{$t('sei.done.toImport')}</button>
        {/if}
        <button class="btn !py-1.5" onclick={reset}><RotateCcw size={14} />{$t('sei.done.new')}</button>
      </div>
    </div>
  {/if}
</section>

<!-- Plan B (ADB): ręczna instalacja agenta z Download/ -->
{#if manualOpen && running}
  <div class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 p-6 backdrop-blur-sm" role="presentation">
    <div class="card w-full max-w-lg animate-fade-in space-y-5 border-warn/50" role="dialog" aria-modal="true">
      <div class="flex items-center gap-3">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl border border-warn/50 bg-warn/10 text-warn"><PackageOpen size={24} /></div>
        <h3 class="text-base font-semibold text-neutral-100">{$t('backup.manual.title')}</h3>
      </div>
      <p class="text-sm leading-relaxed text-neutral-200">{$t('backup.manual.text')}</p>
      <ol class="space-y-2 text-sm text-neutral-300">
        <li class="flex items-start gap-3"><FolderOpen size={17} class="mt-0.5 shrink-0 text-gold" />{$t('backup.manual.tip1')}</li>
        <li class="flex items-start gap-3"><ShieldCheck size={17} class="mt-0.5 shrink-0 text-gold" />{$t('backup.manual.tip2')}</li>
      </ol>
      {#if manualReason}<p class="select-text rounded-lg border border-ink-600 bg-ink-900 p-2.5 text-xs text-neutral-400">{manualReason}</p>{/if}
      <div class="flex justify-end gap-2 border-t border-ink-600 pt-4">
        <button class="btn btn-danger !py-1.5" onclick={() => job.cancel()}><Square size={14} />{$t('common.cancel')}</button>
        <button class="btn btn-gold !py-1.5" disabled={manualChecking} onclick={confirmManual}>
          {#if manualChecking}<LoaderCircle size={14} class="animate-spin" />{$t('backup.manual.checking')}{:else}<CircleCheck size={14} />{$t('backup.manual.done')}{/if}
        </button>
      </div>
    </div>
  </div>
{/if}
