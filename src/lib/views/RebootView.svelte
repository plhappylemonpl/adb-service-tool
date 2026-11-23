<script lang="ts">
  import { Power, RotateCcw, LifeBuoy, Download, Zap, Cpu, Radio, PowerOff, TriangleAlert } from 'lucide-svelte';
  import { api, type RebootMode } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import { confirmDialog } from '$lib/components/Confirm.svelte';

  const dev = $derived(selectedDevice());
  const via = $derived<'adb' | 'fastboot'>(dev?.mode === 'fastboot' ? 'fastboot' : 'adb');
  const usable = $derived(!!dev && ['adb', 'recovery', 'sideload', 'fastboot'].includes(dev.mode));
  let busy = $state<RebootMode | null>(null);

  const modes: { id: RebootMode; icon: typeof Power; adb: boolean; fb: boolean; danger?: boolean }[] = [
    { id: 'system', icon: RotateCcw, adb: true, fb: true },
    { id: 'recovery', icon: LifeBuoy, adb: true, fb: true },
    { id: 'download', icon: Download, adb: true, fb: false },
    { id: 'bootloader', icon: Zap, adb: true, fb: true },
    { id: 'fastbootd', icon: Cpu, adb: true, fb: true },
    { id: 'sideload', icon: Radio, adb: true, fb: false },
    { id: 'edl', icon: TriangleAlert, adb: true, fb: true, danger: true },
    { id: 'poweroff', icon: PowerOff, adb: true, fb: true }
  ];

  async function go(m: RebootMode, danger?: boolean) {
    if (!dev) return;
    if (danger) {
      const ok = await confirmDialog({ title: $t('reboot.edl'), message: $t('reboot.edlWarn'), danger: true });
      if (!ok) return;
    }
    busy = m;
    try {
      await api.reboot(dev.serial, m, via);
      pushLog('ok', `${$t('reboot.done')} → ${$t('reboot.' + m)} [${dev.serial}]`, 'power');
    } catch (e) {
      pushLog('error', errText(e), 'power');
    } finally {
      busy = null;
    }
  }
</script>

<section class="animate-fade-in space-y-4">
  <h2 class="view-title">{$t('reboot.title')}</h2>

  {#if !usable}
    <div class="card text-sm text-warn">{$t('reboot.needDevice')}</div>
  {:else}
    <div class="card flex items-center gap-2 text-sm text-neutral-400">
      <Power size={15} class="text-gold" />{$t('reboot.via')}: <span class="font-mono text-gold">{via.toUpperCase()}</span>
      <span class="ml-auto text-xs text-warn">{$t('reboot.warn')}</span>
    </div>
    <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      {#each modes as m (m.id)}
        {@const enabled = via === 'fastboot' ? m.fb : m.adb}
        <button
          class="card group flex flex-col items-start gap-2 text-left transition-all hover:border-gold disabled:pointer-events-none disabled:opacity-35 {m.danger ? 'hover:!border-err' : ''}"
          disabled={!enabled || busy !== null}
          onclick={() => go(m.id, m.danger)}
        >
          <div class="flex w-full items-center justify-between">
            <m.icon size={22} class="{m.danger ? 'text-err' : 'text-gold'} {busy === m.id ? 'animate-pulse' : ''}" />
          </div>
          <div class="text-sm font-semibold text-neutral-100">{$t('reboot.' + m.id)}</div>
          <div class="text-xs text-neutral-500">{$t('reboot.d.' + m.id)}</div>
        </button>
      {/each}
    </div>
  {/if}
</section>
