import { writable, derived, get } from 'svelte/store';
import pl from './locales/pl.json';
import en from './locales/en.json';

export type Lang = 'pl' | 'en';
type Dict = Record<string, string>;

const dicts: Record<Lang, Dict> = { pl, en };

const STORAGE_KEY = 'adbst.lang';

function initialLang(): Lang {
  try {
    const saved = localStorage.getItem(STORAGE_KEY) as Lang | null;
    if (saved === 'pl' || saved === 'en') return saved;
    // domyślnie język systemu, fallback PL (sklep w Polsce)
    const nav = navigator.language?.toLowerCase() ?? 'pl';
    return nav.startsWith('en') ? 'en' : 'pl';
  } catch {
    return 'pl';
  }
}

export const lang = writable<Lang>(initialLang());

lang.subscribe((l) => {
  try {
    localStorage.setItem(STORAGE_KEY, l);
    document.documentElement.lang = l;
  } catch {
    /* brak localStorage (np. prerender) — ignoruj */
  }
});

/** Reaktywna funkcja tłumacząca: $t('nav.pull'). */
export const t = derived(lang, ($lang) => {
  const d = dicts[$lang] ?? dicts.pl;
  return (key: string, fallback?: string): string => d[key] ?? fallback ?? key;
});

/** Nie-reaktywne tłumaczenie (np. w logice JS). */
export function tr(key: string, fallback?: string): string {
  const d = dicts[get(lang)] ?? dicts.pl;
  return d[key] ?? fallback ?? key;
}

export function toggleLang() {
  lang.update((l) => (l === 'pl' ? 'en' : 'pl'));
}
