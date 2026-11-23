<script lang="ts">
  import {
    Rocket, Globe, SkipForward, Languages, TerminalSquare, Play, Zap, ShieldAlert, Usb, Boxes, Wrench
  } from 'lucide-svelte';
  import { api, type AdbActionReq, type ShellOutput } from '$lib/api';
  import { selectedDevice, pushLog } from '$lib/stores.svelte';
  import { t } from '$lib/i18n';
  import { errText } from '$lib/format';
  import { confirmDialog } from '$lib/components/Confirm.svelte';

  const dev = $derived(selectedDevice());
  const inAdb = $derived(!!dev && (dev.mode === 'adb' || dev.mode === 'recovery'));
  const inFastboot = $derived(!!dev && dev.mode === 'fastboot');

  let out = $state<{ cmd: string; text: string; err: boolean }[]>([]);
  let busy = $state(false);

  function log(cmd: string, r: ShellOutput | string) {
    const text = typeof r === 'string' ? r : `${r.stdout}${r.stderr ? '\n' + r.stderr : ''}`.trim();
    const err = typeof r === 'string' ? false : r.status !== 0;
    out.unshift({ cmd, text: text || '(ok)', err });
    if (out.length > 40) out.pop();
  }

  async function runAdb(cmd: string, action: AdbActionReq, danger = false) {
    if (!dev) return;
    if (danger) {
      const ok = await confirmDialog({ title: cmd, message: $t('ent.confirm'), danger: true, confirmLabel: $t('common.confirm') });
      if (!ok) return;
    }
    busy = true;
    try {
      log(cmd, await api.enterpriseAdb(dev.serial, action));
    } catch (e) {
      log(cmd, errText(e));
    } finally {
      busy = false;
    }
  }

  async function runFastboot(id: string, label: string, danger: boolean) {
    if (!dev) return;
    if (danger) {
      const ok = await confirmDialog({ title: label, message: $t('ent.fbConfirm'), danger: true, confirmLabel: label });
      if (!ok) return;
    }
    busy = true;
    try {
      log(label, await api.enterpriseFastboot(dev.serial, id));
    } catch (e) {
      log(label, errText(e));
    } finally {
      busy = false;
    }
  }

  async function mtpReset() {
    busy = true;
    try {
      log('MTP ResetDevice', await api.enterpriseMtpReset());
    } catch (e) {
      log('MTP ResetDevice', errText(e));
    } finally {
      busy = false;
    }
  }

  // --- pola formularzy ---
  let url = $state('https://');
  let secret = $state('4636');
  let locale = $state('en-US');
  let component = $state('com.android.settings/.Settings');
  let amRaw = $state('start -a android.intent.action.MAIN');

  const fastbootCmds: { id: string; label: string; danger: boolean }[] = [
    { id: 'deviceInfo', label: 'oem device-info', danger: false },
    { id: 'getVarAll', label: 'getvar all', danger: false },
    { id: 'rebootRecovery', label: 'oem reboot-recovery', danger: false },
    { id: 'rebootBootloader', label: 'reboot-bootloader', danger: false },
    { id: 'rebootFastbootd', label: 'reboot fastbootd', danger: false },
    { id: 'rebootDownload', label: 'reboot download', danger: false },
    { id: 'oemEdl', label: 'oem edl', danger: true },
    { id: 'rebootEdl', label: 'reboot edl', danger: true },
    { id: 'eraseConfig', label: 'erase config', danger: true },
    { id: 'erasePersistent', label: 'erase persistent', danger: true },
    { id: 'eraseMetadata', label: 'erase metadata', danger: true },
    { id: 'eraseCarrier', label: 'erase carrier', danger: true },
    { id: 'oemUnlock', label: 'oem unlock', danger: true },
    { id: 'flashingUnlock', label: 'flashing unlock', danger: true },
    { id: 'oemLock', label: 'oem lock', danger: true },
    { id: 'flashingLock', label: 'flashing lock', danger: true }
  ];
</script>

<section class="animate-fade-in space-y-4">
  <div>
    <h2 class="view-title">{$t('ent.title')}</h2>
    <p class="text-xs text-neutral-500">{$t('ent.sub')}</p>
  </div>

  <div class="grid gap-4 2xl:grid-cols-[1fr_380px]">
    <div class="space-y-4">
      <!-- ADB Zero-Touch -->
      <div class="card space-y-3">
        <div class="flex items-center gap-2 text-sm font-medium text-gold"><Rocket size={16} />{$t('ent.adbTitle')}</div>
        {#if !inAdb}<div class="text-xs text-warn">{$t('common.needAdb')}</div>{/if}

        <div class="grid gap-2 sm:grid-cols-2">
          <button class="btn !py-2 text-sm" disabled={!inAdb || busy} onclick={() => runAdb('skip setup wizard', { kind: 'skipSetupWizard' }, true)}>
            <SkipForward size={15} />{$t('ent.skipSetup')}
          </button>
          <button class="btn !py-2 text-sm" disabled={!inAdb || busy} onclick={() => runAdb('locale settings', { kind: 'localeSettings' })}>
            <Languages size={15} />{$t('ent.localeSettings')}
          </button>
        </div>

        <div class="space-y-1.5">
          <span class="label">{$t('ent.secret')}</span>
          <div class="flex gap-2">
            <input class="input flex-1 font-mono" bind:value={secret} placeholder="4636" />
            <button class="btn" disabled={!inAdb || busy} onclick={() => runAdb(`secret ${secret}`, { kind: 'secretCode', code: secret })}><Play size={15} /></button>
          </div>
          <div class="flex flex-wrap gap-1.5">
            {#each [['4636','info'],['0','test'],['2663','touch'],['0283','loopback'],['232338','wifi mac'],['7780','reset'],['34971539','camera']] as [c, lbl] (c)}
              <button class="chip" onclick={() => (secret = c)}>{c} · {lbl}</button>
            {/each}
          </div>
        </div>

        <div class="space-y-1.5">
          <span class="label">{$t('ent.mdmUrl')}</span>
          <div class="flex gap-2">
            <input class="input flex-1 font-mono text-xs" bind:value={url} placeholder="https://mdm.example.com/enroll" />
            <button class="btn" disabled={!inAdb || busy} onclick={() => runAdb('open url', { kind: 'openUrl', url })}><Globe size={15} /></button>
          </div>
        </div>

        <div class="space-y-1.5">
          <span class="label">{$t('ent.locale')}</span>
          <div class="flex gap-2">
            <input class="input flex-1 font-mono" bind:value={locale} placeholder="en-US" />
            <button class="btn btn-danger" disabled={!inAdb || busy} title={$t('ent.localeRootHint')} onclick={() => runAdb(`setprop locale ${locale}`, { kind: 'setLocaleRoot', locale }, true)}>
              <Languages size={15} />{$t('ent.setRoot')}
            </button>
          </div>
          <p class="text-[11px] text-neutral-600">{$t('ent.localeRootHint')}</p>
        </div>

        <div class="space-y-1.5">
          <span class="label">am start -n &lt;component&gt;</span>
          <div class="flex gap-2">
            <input class="input flex-1 font-mono text-xs" bind:value={component} />
            <button class="btn" disabled={!inAdb || busy} onclick={() => runAdb(`start ${component}`, { kind: 'startActivity', component })}><Play size={15} /></button>
          </div>
        </div>

        <div class="space-y-1.5">
          <span class="label">am &lt;…&gt; ({$t('ent.amRawHint')})</span>
          <div class="flex gap-2">
            <span class="self-center font-mono text-sm text-gold">am</span>
            <input class="input flex-1 font-mono text-xs" bind:value={amRaw} />
            <button class="btn" disabled={!inAdb || busy} onclick={() => runAdb(`am ${amRaw}`, { kind: 'amRaw', args: amRaw })}><TerminalSquare size={15} /></button>
          </div>
        </div>
      </div>

      <!-- Fastboot OEM -->
      <div class="card space-y-3">
        <div class="flex items-center gap-2 text-sm font-medium text-gold"><Zap size={16} />{$t('ent.fbTitle')}</div>
        {#if !inFastboot}<div class="text-xs text-warn">{$t('ent.needFastboot')}</div>{/if}
        <div class="flex flex-wrap gap-2">
          {#each fastbootCmds as c (c.id)}
            <button
              class="btn !py-1.5 text-xs {c.danger ? 'btn-danger' : ''}"
              disabled={!inFastboot || busy}
              onclick={() => runFastboot(c.id, `fastboot ${c.label}`, c.danger)}
            >
              {#if c.danger}<ShieldAlert size={13} />{/if}fastboot {c.label}
            </button>
          {/each}
        </div>
      </div>

      <!-- MTP -->
      <div class="card space-y-2">
        <div class="flex items-center gap-2 text-sm font-medium text-gold"><Usb size={16} />{$t('ent.mtpTitle')}</div>
        <p class="text-xs text-neutral-500">{$t('ent.mtpNote')}</p>
        <button class="btn !py-1.5 text-xs" disabled={busy} onclick={mtpReset}><Wrench size={14} />MTP ResetDevice (0x1010)</button>
      </div>
    </div>

    <!-- wyjście -->
    <div class="card flex min-h-[300px] flex-col">
      <div class="mb-2 flex items-center gap-2 text-sm text-neutral-300"><Boxes size={15} class="text-gold" />{$t('ent.output')}</div>
      <div class="flex-1 space-y-3 overflow-y-auto rounded-lg border border-ink-600 bg-black p-3 font-mono text-xs">
        {#each out as o, i (i)}
          <div>
            <div class="text-gold">$ {o.cmd}</div>
            <pre class="select-text whitespace-pre-wrap break-all {o.err ? 'text-err' : 'text-neutral-300'}">{o.text}</pre>
          </div>
        {:else}
          <div class="text-neutral-600">{$t('ent.outputHint')}</div>
        {/each}
      </div>
    </div>
  </div>
</section>
