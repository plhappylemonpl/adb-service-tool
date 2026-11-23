<script lang="ts" module>
  // Globalne powiadomienia (toast): `toast('error', 'Tytuł', 'szczegóły')`.
  export type ToastKind = 'ok' | 'error' | 'warn' | 'info';
  interface ToastItem {
    id: number;
    kind: ToastKind;
    title: string;
    message?: string;
  }
  const toasts = $state<{ items: ToastItem[] }>({ items: [] });
  let seq = 0;

  export function toast(kind: ToastKind, title: string, message?: string, ms?: number) {
    const id = ++seq;
    toasts.items.push({ id, kind, title, message });
    if (toasts.items.length > 4) toasts.items.splice(0, toasts.items.length - 4);
    // błędy wiszą dłużej - trzeba zdążyć przeczytać
    setTimeout(() => dismiss(id), ms ?? (kind === 'error' ? 9000 : 5000));
  }

  export function dismiss(id: number) {
    const i = toasts.items.findIndex((t) => t.id === id);
    if (i >= 0) toasts.items.splice(i, 1);
  }
</script>

<script lang="ts">
  import { CircleCheck, CircleX, TriangleAlert, Info, X } from 'lucide-svelte';

  const icon = { ok: CircleCheck, error: CircleX, warn: TriangleAlert, info: Info };
  const tone = {
    ok: 'border-ok/50 text-ok',
    error: 'border-err/60 text-err',
    warn: 'border-warn/60 text-warn',
    info: 'border-gold/50 text-gold'
  };
</script>

<div class="pointer-events-none fixed bottom-5 right-5 z-[70] flex w-[380px] max-w-[calc(100vw-2.5rem)] flex-col gap-2">
  {#each toasts.items as it (it.id)}
    {@const Icon = icon[it.kind]}
    <div
      class="pointer-events-auto flex animate-fade-in items-start gap-3 rounded-xl border bg-ink-800/95 p-3.5 shadow-2xl backdrop-blur {tone[it.kind]}"
      role={it.kind === 'error' ? 'alert' : 'status'}
    >
      <Icon size={18} class="mt-0.5 shrink-0" />
      <div class="min-w-0 flex-1">
        <div class="text-sm font-medium">{it.title}</div>
        {#if it.message}<div class="mt-0.5 select-text whitespace-pre-line text-xs text-neutral-300">{it.message}</div>{/if}
      </div>
      <button class="icon-btn -m-1 shrink-0" onclick={() => dismiss(it.id)} aria-label="close"><X size={14} /></button>
    </div>
  {/each}
</div>
