<script lang="ts">
  import { Check, ChevronDown, Monitor } from 'lucide-svelte';
  import { LANGS, lang, langSetting, setLang, autoLang, t, type LangSetting } from '$lib/i18n';
  import { FLAGS } from '$lib/flags';

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();

  const current = $derived(LANGS.find((l) => l.code === $lang) ?? LANGS[0]);
  // locale systemu przychodzi z backendu asynchronicznie (initLang)
  const auto = $derived(LANGS.find((l) => l.code === $autoLang) ?? LANGS[0]);

  function pick(s: LangSetting) {
    setLang(s);
    open = false;
  }

  function onDoc(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }

  function onKey(e: KeyboardEvent) {
    if (open && e.key === 'Escape') open = false;
  }

  const short = (code: string) => code.split('-')[0].toUpperCase() + (code.includes('-') ? '-' + code.split('-')[1] : '');
</script>

<svelte:window onmousedown={onDoc} onkeydown={onKey} />

<div class="relative" bind:this={root}>
  <button
    class="btn !py-1.5"
    onclick={() => (open = !open)}
    aria-haspopup="listbox"
    aria-expanded={open}
    title={$t('lang.choose')}
  >
    <img src={FLAGS[current.flag]} alt="" class="h-3.5 w-[18px] rounded-[2px] object-cover ring-1 ring-black/40" />
    <span class="font-mono text-xs">{short(current.code)}</span>
    <ChevronDown size={13} class="text-neutral-500 transition-transform {open ? 'rotate-180' : ''}" />
  </button>

  {#if open}
    <div
      class="absolute right-0 top-full z-[80] mt-1.5 w-64 animate-fade-in overflow-hidden rounded-lg border border-gold/30 bg-ink-800 shadow-[0_12px_40px_rgba(0,0,0,0.6)]"
      role="listbox"
      aria-label={$t('lang.choose')}
    >
      <button
        class="flex w-full items-center gap-2.5 border-b border-ink-600 px-3 py-2 text-left text-sm hover:bg-gold/10 {$langSetting === 'auto' ? 'text-gold' : 'text-neutral-200'}"
        role="option"
        aria-selected={$langSetting === 'auto'}
        onclick={() => pick('auto')}
      >
        <Monitor size={15} class="shrink-0 text-gold" />
        <span class="min-w-0 flex-1">
          <span class="block">{$t('lang.auto')}</span>
          <span class="flex items-center gap-1.5 text-[11px] text-neutral-500">
            <img src={FLAGS[auto.flag]} alt="" class="h-2.5 w-[13px] rounded-[1px] object-cover" />{auto.name}
          </span>
        </span>
        {#if $langSetting === 'auto'}<Check size={14} class="shrink-0" />{/if}
      </button>
      <div class="max-h-[min(60vh,420px)] overflow-y-auto py-1">
        {#each LANGS as l (l.code)}
          {@const sel = $langSetting === l.code}
          <button
            class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-sm hover:bg-gold/10 {sel ? 'bg-gold/5 text-gold' : 'text-neutral-200'}"
            role="option"
            aria-selected={sel}
            lang={l.code}
            onclick={() => pick(l.code)}
          >
            <img src={FLAGS[l.flag]} alt="" class="h-3.5 w-[18px] shrink-0 rounded-[2px] object-cover ring-1 ring-black/40" />
            <span class="min-w-0 flex-1 truncate">{l.name}</span>
            <span class="font-mono text-[10px] text-neutral-600">{l.code}</span>
            {#if sel}<Check size={14} class="shrink-0" />{/if}
          </button>
        {/each}
      </div>
    </div>
  {/if}
</div>
