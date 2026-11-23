<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { FilePlus, FolderPlus, X, Send, Folder, File as FileIcon, Smartphone } from 'lucide-svelte';
  import { api } from '$lib/api';
  import { selectedDevice, Job } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import TreeNode from '$lib/components/TreeNode.svelte';
  import ProgressCard from '$lib/components/ProgressCard.svelte';

  const job = new Job('push');

  // lokalne elementy: ścieżka -> czy folder
  let items = $state<{ path: string; dir: boolean }[]>([]);
  let remoteDest = $state('/sdcard/Download');
  let workers = $state(4);
  let timeout = $state(600);
  let skipExisting = $state(true);
  let treeKey = $state(0);

  const dev = $derived(selectedDevice());
  const ready = $derived(!!dev && (dev.mode === 'adb' || dev.mode === 'recovery'));

  const quickDest = ['/sdcard', '/sdcard/Download', '/sdcard/DCIM', '/sdcard/Pictures', '/sdcard/Documents', '/sdcard/Music', '/sdcard/Movies'];

  function add(paths: string[], dir: boolean) {
    for (const p of paths) if (!items.some((i) => i.path === p)) items.push({ path: p, dir });
  }

  async function addFiles() {
    const r = await open({ multiple: true, directory: false });
    if (Array.isArray(r)) add(r, false);
    else if (typeof r === 'string') add([r], false);
  }

  async function addFolder() {
    const r = await open({ multiple: true, directory: true });
    if (Array.isArray(r)) add(r, true);
    else if (typeof r === 'string') add([r], true);
  }

  async function start() {
    if (!dev || items.length === 0) return;
    await job.start((on) =>
      api.startPush(
        dev.serial,
        { items: items.map((i) => i.path), remoteDest, workers, timeoutSeconds: timeout, skipExisting },
        on
      )
    );
  }
</script>

<section class="animate-fade-in space-y-4">
  <h2 class="view-title">{$t('push.title')}</h2>

  {#if !ready}
    <div class="card text-sm text-warn">{$t('common.needAdb')}</div>
  {:else}
    <div class="grid gap-4 xl:grid-cols-2">
      <!-- źródło -->
      <div class="card flex min-h-[320px] flex-col">
        <div class="mb-2 flex items-center justify-between">
          <span class="text-sm font-medium text-neutral-300">{$t('push.source')} ({items.length})</span>
          <div class="flex gap-2">
            <button class="btn !py-1.5 text-xs" onclick={addFiles}><FilePlus size={14} />{$t('push.addFiles')}</button>
            <button class="btn !py-1.5 text-xs" onclick={addFolder}><FolderPlus size={14} />{$t('push.addFolder')}</button>
          </div>
        </div>
        <div class="flex-1 space-y-0.5 overflow-y-auto rounded-lg border border-ink-600 bg-ink-900 p-2">
          {#each items as it (it.path)}
            <div class="group flex items-center gap-2 rounded px-1 py-1 text-sm hover:bg-ink-700">
              {#if it.dir}<Folder size={15} class="shrink-0 text-gold" />{:else}<FileIcon size={15} class="shrink-0 text-neutral-500" />{/if}
              <span class="flex-1 truncate font-mono text-xs text-neutral-300" title={it.path}>{it.path}</span>
              <button class="opacity-0 group-hover:opacity-100" onclick={() => (items = items.filter((x) => x.path !== it.path))}>
                <X size={14} class="text-err" />
              </button>
            </div>
          {:else}
            <div class="p-4 text-center text-xs text-neutral-600">{$t('push.empty')}</div>
          {/each}
        </div>
        {#if items.length}
          <button class="mt-2 self-end text-xs text-neutral-500 hover:text-err" onclick={() => (items = [])}>{$t('common.clear')}</button>
        {/if}
      </div>

      <!-- cel -->
      <div class="card flex flex-col space-y-3">
        <div>
          <span class="label">{$t('push.dest')}</span>
          <div class="flex items-center gap-2">
            <Smartphone size={15} class="shrink-0 text-gold" />
            <input class="input flex-1 font-mono" bind:value={remoteDest} />
          </div>
        </div>
        <div class="flex flex-wrap gap-1.5">
          {#each quickDest as q (q)}
            <button class="chip {remoteDest === q ? 'chip-on' : ''}" onclick={() => (remoteDest = q)}>{q.replace('/sdcard', '') || '/sdcard'}</button>
          {/each}
        </div>
        <div class="text-xs text-neutral-500">{$t('push.pickHint')}</div>
        <div class="max-h-52 flex-1 overflow-y-auto rounded-lg border border-ink-600 bg-ink-900 p-1">
          {#key treeKey}
            <TreeNode serial={dev!.serial} path="/sdcard" name="/sdcard" isDir={true} picked={remoteDest} onPick={(p) => (remoteDest = p)} showFiles={false} />
          {/key}
        </div>
        <div class="grid grid-cols-2 gap-3">
          <label class="block"><span class="label">{$t('opt.workers')}</span>
            <input type="number" min="1" max="16" bind:value={workers} class="input" /></label>
          <label class="block"><span class="label">{$t('opt.timeout')}</span>
            <input type="number" min="10" bind:value={timeout} class="input" /></label>
        </div>
        <label class="check"><input type="checkbox" bind:checked={skipExisting} />{$t('push.skipExisting')}</label>
        <button class="btn btn-gold w-full justify-center py-3 text-base" disabled={job.running || items.length === 0 || !remoteDest.startsWith('/')} onclick={start}>
          <Send size={17} />{$t('push.start')}
        </button>
      </div>
    </div>

    <ProgressCard {job} />
  {/if}
</section>
