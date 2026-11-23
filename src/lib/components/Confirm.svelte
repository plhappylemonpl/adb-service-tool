<script lang="ts" module>
  // Globalny, promisowy dialog potwierdzenia: `if (await confirmDialog({...})) ...`
  interface ConfirmReq {
    title: string;
    message: string;
    items?: string[];
    danger?: boolean;
    confirmLabel?: string;
    resolve: (v: boolean) => void;
  }
  const req = $state<{ cur: ConfirmReq | null }>({ cur: null });

  export function confirmDialog(o: Omit<ConfirmReq, 'resolve'>): Promise<boolean> {
    return new Promise((resolve) => {
      req.cur = { ...o, resolve };
    });
  }
</script>

<script lang="ts">
  import { TriangleAlert } from 'lucide-svelte';
  import { t } from '$lib/i18n';

  function close(v: boolean) {
    req.cur?.resolve(v);
    req.cur = null;
  }

  function onKey(e: KeyboardEvent) {
    if (!req.cur) return;
    if (e.key === 'Escape') close(false);
  }
</script>

<svelte:window onkeydown={onKey} />

{#if req.cur}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6 backdrop-blur-sm">
    <div class="card w-full max-w-lg animate-fade-in space-y-4 border-ink-400 shadow-2xl">
      <div class="flex items-center gap-2 text-base font-semibold {req.cur.danger ? 'text-err' : 'text-gold'}">
        <TriangleAlert size={18} />{req.cur.title}
      </div>
      <p class="whitespace-pre-line text-sm text-neutral-300">{req.cur.message}</p>
      {#if req.cur.items?.length}
        <div class="max-h-48 overflow-y-auto rounded border border-ink-600 bg-ink-900 p-2 font-mono text-xs text-neutral-400">
          {#each req.cur.items as it (it)}<div>{it}</div>{/each}
        </div>
      {/if}
      <div class="flex justify-end gap-2">
        <button class="btn" onclick={() => close(false)}>{$t('common.cancel')}</button>
        <button class="btn {req.cur.danger ? 'btn-danger-solid' : 'btn-gold'}" onclick={() => close(true)}>
          {req.cur.confirmLabel ?? $t('common.confirm')}
        </button>
      </div>
    </div>
  </div>
{/if}
