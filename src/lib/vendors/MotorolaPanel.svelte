<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { FolderOpen, Zap, FileCode, CircleCheck, CircleX, TriangleAlert } from 'lucide-svelte';
  import { api, type MotoFirmware } from '$lib/api';
  import { selectedDevice, Job, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText, humanBytes } from '$lib/format';
  import ProgressCard from '$lib/components/ProgressCard.svelte';
  import { confirmDialog } from '$lib/components/Confirm.svelte';

  const job = new Job('moto');
  const dev = $derived(selectedDevice());
  const inFastboot = $derived(!!dev && dev.mode === 'fastboot');

  let fw = $state<MotoFirmware | null>(null);
  let loading = $state(false);
  let keepData = $state(true);
  let verifyMd5 = $state(true);
  let rebootAfter = $state(true);

  async function load(dir: string, xml: string | null) {
    loading = true;
    try {
      fw = await api.motoLoad(dir, xml);
      keepData = fw.wipesData; // domyślnie bezpiecznie: zachowaj dane
      pushLog('info', `${fw.xmlFile}: ${fw.steps.length} ${$t('moto.steps')}`, 'moto');
    } catch (e) {
      fw = null;
      pushLog('error', errText(e), 'moto');
    } finally {
      loading = false;
    }
  }

  async function pick() {
    const d = await open({ directory: true, multiple: false, title: 'flashfile.xml / servicefile.xml' });
    if (typeof d === 'string') await load(d, null);
  }

  const effectiveSteps = $derived(fw ? fw.steps.filter((s) => !(keepData && s.wipesData)) : []);

  async function flash() {
    if (!dev || !fw) return;
    const wipes = fw.wipesData && !keepData;
    const ok = await confirmDialog({
      title: $t('moto.confirmTitle'),
      message:
        `${fw.phoneModel ?? '?'} • ${fw.softwareVersion ?? '?'}\n${fw.xmlFile} → ${dev.serial}\n\n` +
        $t('moto.confirmText') +
        (wipes ? '\n\n' + $t('moto.confirmWipe') : ''),
      items: effectiveSteps.map((s) => s.command),
      danger: true,
      confirmLabel: $t('moto.flash')
    });
    if (!ok) return;
    await job.start((on) =>
      api.motoFlash(dev.serial, { dir: fw!.dir, xmlFile: fw!.xmlFile, keepData, verifyMd5, rebootAfter }, on)
    );
  }
</script>

<div class="space-y-4">
  <div class="card space-y-3">
    <div class="flex items-center justify-between gap-3">
      <div class="flex items-center gap-2 text-sm font-medium text-gold"><FileCode size={16} />{$t('moto.title')}</div>
      <button class="btn" onclick={pick} disabled={loading || job.running}><FolderOpen size={15} />{$t('moto.pick')}</button>
    </div>

    {#if fw}
      <div class="grid gap-2 text-sm sm:grid-cols-2 xl:grid-cols-4">
        <div><span class="text-neutral-500">{$t('moto.model')}:</span> <span class="font-mono text-gold">{fw.phoneModel ?? '—'}</span></div>
        <div class="truncate" title={fw.softwareVersion ?? ''}><span class="text-neutral-500">{$t('moto.version')}:</span> <span class="font-mono text-neutral-200">{fw.softwareVersion ?? '—'}</span></div>
        <div><span class="text-neutral-500">XML:</span>
          <select class="input !inline-block !w-auto !py-1 text-xs" value={fw.xmlFile} onchange={(e) => load(fw!.dir, (e.target as HTMLSelectElement).value)}>
            {#each [fw.xmlFile, ...fw.alternatives] as x (x)}<option value={x}>{x}</option>{/each}
          </select>
        </div>
        <div><span class="text-neutral-500">{$t('moto.size')}:</span> <span class="font-mono text-neutral-200">{humanBytes(fw.totalBytes)}</span></div>
      </div>

      {#if fw.missingFiles.length}
        <div class="flex items-start gap-2 rounded-lg border border-err/40 bg-err/10 p-3 text-xs text-err">
          <TriangleAlert size={14} class="mt-px shrink-0" />{$t('moto.missing')}: {fw.missingFiles.join(', ')}
        </div>
      {/if}

      <div class="flex flex-wrap gap-x-6 gap-y-2">
        <label class="toggle"><input type="checkbox" bind:checked={keepData} disabled={!fw.wipesData} /><span></span>{$t('moto.keepData')}</label>
        <label class="toggle"><input type="checkbox" bind:checked={verifyMd5} /><span></span>{$t('moto.verify')}</label>
        <label class="toggle"><input type="checkbox" bind:checked={rebootAfter} /><span></span>{$t('moto.reboot')}</label>
      </div>

      <div class="max-h-[38vh] overflow-y-auto rounded-lg border border-ink-600 bg-black">
        {#each fw.steps as s (s.index)}
          {@const skipped = keepData && s.wipesData}
          <div class="flex items-center gap-3 border-b border-ink-700 px-3 py-1.5 font-mono text-xs last:border-0 {skipped ? 'opacity-35 line-through' : ''}">
            <span class="w-7 shrink-0 text-right text-neutral-600">{s.index + 1}</span>
            <span class="flex-1 truncate {s.op === 'flash' ? 'text-gold' : s.op === 'erase' ? 'text-warn' : 'text-neutral-400'}">{s.command}</span>
            {#if s.size !== null}<span class="text-neutral-600">{humanBytes(s.size)}</span>{/if}
            {#if s.filename}
              {#if s.fileExists}<CircleCheck size={13} class="text-ok" />{:else}<CircleX size={13} class="text-err" />{/if}
            {/if}
            {#if s.wipesData}<span class="badge bg-err/15 text-err">wipe</span>{/if}
          </div>
        {/each}
      </div>

      {#if !inFastboot}
        <div class="text-sm text-warn">{$t('moto.needFastboot')}</div>
      {/if}
      <button
        class="btn btn-danger-solid w-full justify-center py-3 text-base"
        disabled={!inFastboot || job.running || fw.missingFiles.length > 0}
        onclick={flash}
      >
        <Zap size={17} />{$t('moto.flash')} ({effectiveSteps.length} {$t('moto.steps')})
      </button>
      <p class="text-[11px] text-neutral-600">{$t('moto.safety')}</p>
    {:else}
      <p class="text-sm text-neutral-500">{$t('moto.hint')}</p>
    {/if}
  </div>
  <ProgressCard {job} />
</div>
