<script lang="ts">
  import { ChevronRight, ChevronDown, Folder, FolderOpen, File as FileIcon, Lock, LoaderCircle } from 'lucide-svelte';
  import type { SvelteMap } from 'svelte/reactivity';
  import { api } from '$lib/api';
  import { t } from '$lib/i18n';
  import { basename } from '$lib/format';
  import Self from './TreeNode.svelte';

  // Rekurencyjny, leniwie ładowany węzeł drzewa plików urządzenia.
  // Tryb "select": checkboxy (pobieranie). Tryb "pick": klik w folder = wybór celu (wysyłanie).
  let {
    serial,
    path,
    name,
    isDir,
    depth = 0,
    selected = null,
    picked = null,
    onPick = null,
    showFiles = true,
    root = false
  }: {
    serial: string;
    path: string;
    name: string;
    isDir: boolean;
    depth?: number;
    selected?: SvelteMap<string, boolean> | null; // path -> isFile
    picked?: string | null;
    onPick?: ((p: string) => void) | null;
    showFiles?: boolean;
    /** listuj z uprawnieniami root (su) - tylko gdy wykryto root */
    root?: boolean;
  } = $props();

  let expanded = $state(false);
  let loading = $state(false);
  let loaded = $state(false);
  let noPerm = $state(false);
  let dirs = $state<string[]>([]);
  let files = $state<string[]>([]);

  const checked = $derived(selected?.has(path) ?? false);
  const isPicked = $derived(picked === path);

  async function toggleExpand() {
    if (!isDir) return;
    expanded = !expanded;
    if (expanded && !loaded && !loading) {
      loading = true;
      try {
        const r = await api.listDir(serial, path, root);
        dirs = r.dirs.sort((a, b) => a.localeCompare(b));
        files = r.files.sort((a, b) => a.localeCompare(b));
        loaded = true;
      } catch {
        noPerm = true;
      } finally {
        loading = false;
      }
    }
  }

  function toggleCheck() {
    if (!selected) return;
    if (selected.has(path)) selected.delete(path);
    else selected.set(path, !isDir);
  }

  function onRowClick() {
    if (onPick && isDir) onPick(path);
  }
</script>

<div>
  <div
    class="group flex items-center gap-1.5 rounded px-1 py-[3px] hover:bg-ink-700 {isPicked ? 'bg-gold/10 ring-1 ring-gold/50' : ''}"
    style="padding-left: {depth * 16 + 4}px"
  >
    {#if isDir}
      <button class="text-neutral-500 hover:text-gold" onclick={toggleExpand} aria-label="expand">
        {#if loading}
          <LoaderCircle size={14} class="animate-spin" />
        {:else if expanded}
          <ChevronDown size={14} />
        {:else}
          <ChevronRight size={14} />
        {/if}
      </button>
    {:else}
      <span class="w-[14px]"></span>
    {/if}

    {#if selected}
      <input type="checkbox" checked={checked} onchange={toggleCheck} class="accent-[#c9a249]" />
    {/if}

    <button
      class="flex min-w-0 flex-1 items-center gap-1.5 text-left"
      ondblclick={toggleExpand}
      onclick={onRowClick}
    >
      {#if isDir}
        {#if expanded}<FolderOpen size={15} class="shrink-0 text-gold" />{:else}<Folder size={15} class="shrink-0 text-gold" />{/if}
      {:else}
        <FileIcon size={15} class="shrink-0 text-neutral-500" />
      {/if}
      <span class="truncate text-sm {checked || isPicked ? 'text-gold' : 'text-neutral-300'}">{name}</span>
    </button>

    {#if noPerm}
      <Lock size={13} class="ml-auto shrink-0 text-err" />
      <span class="shrink-0 text-xs text-err">{$t('tree.noPerm')}</span>
    {/if}
  </div>

  {#if expanded && loaded}
    {#each dirs as d (d)}
      <Self {serial} path={d} name={basename(d)} isDir={true} depth={depth + 1} {selected} {picked} {onPick} {showFiles} {root} />
    {/each}
    {#if showFiles}
      {#each files as f (f)}
        <Self {serial} path={f} name={basename(f)} isDir={false} depth={depth + 1} {selected} {picked} {onPick} {showFiles} {root} />
      {/each}
    {/if}
    {#if dirs.length === 0 && (files.length === 0 || !showFiles)}
      <div class="py-0.5 text-xs italic text-neutral-600" style="padding-left: {(depth + 1) * 16 + 24}px">{$t('tree.empty')}</div>
    {/if}
  {/if}
</div>
