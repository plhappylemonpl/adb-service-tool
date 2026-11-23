<script lang="ts">
  import { SvelteMap } from 'svelte/reactivity';
  import { open } from '@tauri-apps/plugin-dialog';
  import { HardDrive, Play, X, Folder, File as FileIcon, Sparkles, RefreshCw, ShieldCheck, Crown, LoaderCircle } from 'lucide-svelte';
  import { api, type Partition, type RootInfo } from '$lib/api';
  import { app, selectedDevice, Job, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import TreeNode from '$lib/components/TreeNode.svelte';
  import ProgressCard from '$lib/components/ProgressCard.svelte';

  const job = new Job('pull');

  let dest = $state('');
  let workers = $state(4);
  let timeout = $state(600);
  let resume = $state(true);
  let skipExisting = $state(true);
  let preserveTime = $state(true);
  // Windows zawsze wymaga nazw zgodnych z NTFS - tam przełącznik jest zablokowany
  const isWindows = typeof navigator !== 'undefined' && /windows/i.test(navigator.userAgent);
  const NTFS_KEY = 'adbst.pull.ntfs';
  let ntfsSafe = $state(readNtfs());

  function readNtfs(): boolean {
    if (isWindows) return true;
    try {
      return localStorage.getItem(NTFS_KEY) !== '0';
    } catch {
      return true;
    }
  }

  $effect(() => {
    try {
      localStorage.setItem(NTFS_KEY, ntfsSafe ? '1' : '0');
    } catch {
      /* zapamiętanie wyboru to tylko wygoda */
    }
  });
  const selected = new SvelteMap<string, boolean>(); // path -> isFile

  let roots = $state<string[]>(['/sdcard']);

  // --- root (su / adbd root) ---
  let useRoot = $state(true);
  let rootInfo = $state<RootInfo | null>(null);
  let rootChecking = $state(false);
  const rootActive = $derived(useRoot && !!rootInfo && rootInfo.mode !== 'none');

  async function checkRoot(force = false) {
    if (!dev || !ready) return;
    rootChecking = true;
    try {
      rootInfo = await api.detectRoot(dev.serial, force);
      if (rootInfo.mode !== 'none') pushLog('ok', `${$t('pull.rootActive')} (${rootInfo.mode})`, 'pull');
      else if (rootInfo.suPresent) pushLog('warn', rootInfo.detail, 'pull');
    } catch (e) {
      pushLog('warn', errText(e), 'pull');
    } finally {
      rootChecking = false;
    }
  }

  // drzewo: przy aktywnym roocie dochodzi /data (dane aplikacji)
  const treeRoots = $derived(rootActive ? [...roots, '/data'] : roots);
  let partitions = $state<Partition[]>([]);
  let treeKey = $state(0);

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && (dev.mode === 'adb' || dev.mode === 'recovery'));

  const presets: [string, string][] = [
    ['DCIM', '/sdcard/DCIM'],
    ['Pictures', '/sdcard/Pictures'],
    ['Download', '/sdcard/Download'],
    ['Documents', '/sdcard/Documents'],
    ['Movies', '/sdcard/Movies'],
    ['Music', '/sdcard/Music'],
    ['WhatsApp', '/sdcard/Android/media/com.whatsapp/WhatsApp'],
    ['Recordings', '/sdcard/Recordings']
  ];

  // przy zmianie urządzenia: wyczyść wybór, wykryj pamięci (karta SD)
  let lastSerial: string | null = null;
  $effect(() => {
    const s = dev?.serial ?? null;
    if (s !== lastSerial) {
      lastSerial = s;
      selected.clear();
      roots = ['/sdcard'];
      partitions = [];
      rootInfo = null;
      treeKey++;
      if (s && ready) {
        scanStorage();
        checkRoot();
      }
    }
  });

  async function scanStorage() {
    if (!dev) return;
    try {
      partitions = await api.scanPartitions(dev.serial);
      const extra = partitions
        .map((p) => p.mount)
        .filter((m) => /^\/storage\/[0-9A-F]{4}-[0-9A-F]{4}$/i.test(m));
      roots = ['/sdcard', ...extra];
    } catch (e) {
      pushLog('warn', errText(e), 'pull');
    }
  }

  async function pickDest() {
    const d = await open({ directory: true, multiple: false });
    if (typeof d === 'string') dest = d;
  }

  // backend potwierdza root przed skanem (zdarzenie "root")
  job.onItem = (id, ok, message) => {
    if (id === 'root' && rootInfo && !ok && message === 'none' && useRoot) rootInfo = { ...rootInfo, mode: 'none' };
  };

  async function start() {
    if (!dev || !dest || selected.size === 0) return;
    const items = Array.from(selected.entries()).map(([path, isFile]) => ({ path, isFile }));
    await job.start((on) =>
      api.startPull(
        dev.serial,
        { items, localRoot: dest, workers, timeoutSeconds: timeout, resume, skipExisting, preserveTime, ntfsSafe: isWindows || ntfsSafe, useRoot },
        on
      )
    );
  }
</script>

<section class="animate-fade-in space-y-4">
  <div class="flex flex-wrap items-center justify-between gap-3">
    <h2 class="view-title">{$t('pull.title')}</h2>
    {#if ready}
      {#if rootChecking}
        <span class="flex items-center gap-2 text-xs text-neutral-500"><LoaderCircle size={14} class="animate-spin" />{$t('pull.rootChecking')}</span>
      {:else if rootActive}
        <span class="flex items-center gap-2 rounded-full border border-gold/60 bg-gold/15 px-3 py-1 text-xs font-medium text-gold shadow-[0_0_12px_rgba(201,162,73,0.25)]">
          <Crown size={14} />{$t('pull.rootActive')}
          <span class="font-mono text-[10px] opacity-70">{rootInfo?.mode}</span>
        </span>
      {:else if rootInfo?.suPresent}
        <button class="chip" onclick={() => checkRoot(true)} title={rootInfo.detail}>{$t('pull.rootDenied')}</button>
      {/if}
    {/if}
  </div>

  {#if !ready}
    <div class="card text-sm text-warn">{$t('common.needAdb')}</div>
  {:else}
    <div class="grid gap-4 xl:grid-cols-[1fr_320px]">
      <!-- drzewo -->
      <div class="card flex min-h-[340px] flex-col">
        <div class="mb-2 flex items-center justify-between gap-2">
          <span class="text-sm font-medium text-neutral-300">{$t('pull.tree')}</span>
          <div class="flex items-center gap-2">
            <span class="hidden text-xs text-neutral-500 lg:inline">{$t('pull.expandHint')}</span>
            <button class="icon-btn" title={$t('common.refresh')} onclick={() => { treeKey++; scanStorage(); }}><RefreshCw size={14} /></button>
          </div>
        </div>
        <div class="mb-2 flex flex-wrap gap-1.5">
          <Sparkles size={14} class="mt-1 text-gold" />
          {#each presets as p (p[1])}
            <button
              class="chip {selected.has(p[1]) ? 'chip-on' : ''}"
              onclick={() => (selected.has(p[1]) ? selected.delete(p[1]) : selected.set(p[1], false))}
            >{p[0]}</button>
          {/each}
        </div>
        <div class="max-h-[46vh] flex-1 overflow-y-auto rounded-lg border border-ink-600 bg-ink-900 p-1">
          {#key `${treeKey}-${rootActive}`}
            {#each treeRoots as r (r)}
              <TreeNode serial={dev!.serial} path={r} name={r} isDir={true} {selected} root={rootActive} />
            {/each}
          {/key}
        </div>
      </div>

      <!-- ustawienia -->
      <div class="space-y-4">
        <div class="card space-y-3">
          <div>
            <span class="label">{$t('pull.dest')}</span>
            <div class="flex gap-2">
              <input class="input flex-1" bind:value={dest} placeholder={$t('pull.destPh')} />
              <button class="btn !px-3" onclick={pickDest} title={$t('pull.pickDest')}><HardDrive size={15} /></button>
            </div>
          </div>
          <div class="grid grid-cols-2 gap-3">
            <label class="block"><span class="label">{$t('opt.workers')}</span>
              <input type="number" min="1" max="16" bind:value={workers} class="input" /></label>
            <label class="block"><span class="label">{$t('opt.timeout')}</span>
              <input type="number" min="10" bind:value={timeout} class="input" /></label>
          </div>
          <label class="check"><input type="checkbox" bind:checked={resume} />{$t('pull.resume')}</label>
          <label class="check"><input type="checkbox" bind:checked={skipExisting} />{$t('pull.skipExisting')}</label>
          <label class="check"><input type="checkbox" bind:checked={preserveTime} />{$t('pull.preserveTime')}</label>
          <div class="flex flex-col gap-1.5 border-t border-ink-600 pt-3">
            <label class="toggle" title={$t('pull.ntfsHint')}>
              <input type="checkbox" bind:checked={ntfsSafe} disabled={isWindows || job.running} /><span></span>
              <ShieldCheck size={14} class={ntfsSafe ? 'text-gold' : 'text-neutral-500'} />{$t('pull.ntfs')}
            </label>
            <p class="text-[11px] leading-snug text-neutral-500">{isWindows ? $t('pull.ntfsForced') : $t('pull.ntfsHint')}</p>
          </div>
          <div class="flex flex-col gap-1.5 border-t border-ink-600 pt-3">
            <label class="toggle">
              <input type="checkbox" bind:checked={useRoot} disabled={job.running} /><span></span>
              <Crown size={14} class={rootActive ? 'text-gold' : 'text-neutral-500'} />{$t('pull.useRoot')}
            </label>
            <p class="text-[11px] leading-snug text-neutral-500">
              {rootActive ? $t('pull.rootHintOn') : rootInfo?.suPresent ? $t('pull.rootHintDenied') : $t('pull.rootHintOff')}
            </p>
          </div>
        </div>

        <div class="card">
          <div class="mb-2 flex items-center justify-between">
            <span class="text-sm text-neutral-300">{$t('pull.selected')} ({selected.size})</span>
            {#if selected.size}<button class="text-xs text-neutral-500 hover:text-err" onclick={() => selected.clear()}>{$t('common.clear')}</button>{/if}
          </div>
          <div class="max-h-40 space-y-0.5 overflow-y-auto">
            {#each Array.from(selected.entries()) as entry (entry[0])}
              <div class="group flex items-center gap-1.5 text-xs">
                {#if entry[1]}<FileIcon size={12} class="shrink-0 text-neutral-500" />{:else}<Folder size={12} class="shrink-0 text-gold" />{/if}
                <span class="flex-1 truncate font-mono text-neutral-300" title={entry[0]}>{entry[0]}</span>
                <button class="opacity-0 group-hover:opacity-100" onclick={() => selected.delete(entry[0])}><X size={12} class="text-err" /></button>
              </div>
            {:else}
              <div class="text-xs text-neutral-600">{$t('pull.nothing')}</div>
            {/each}
          </div>
        </div>

        <button
          class="btn btn-gold w-full justify-center py-3 text-base"
          disabled={job.running || !dest || selected.size === 0}
          onclick={start}
        >
          <Play size={17} />{$t('pull.start')}
        </button>
      </div>
    </div>

    <ProgressCard {job} />
  {/if}
</section>
