import { writable, derived, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import en from './locales/en.json';
import { LANGS, FALLBACK, matchLocale } from './localeMatch.js';

export { LANGS };
export type Lang = string;
/** 'auto' = język systemu */
export type LangSetting = Lang | 'auto';
type Dict = Record<string, string>;

// wszystkie słowniki w paczce aplikacji (offline, bez mignięcia angielskim przy starcie)
const files = import.meta.glob<Dict>('./locales/*.json', { eager: true, import: 'default' });
const dicts: Record<string, Dict> = {};
for (const [path, dict] of Object.entries(files)) {
  const code = path.split('/').pop()!.replace('.json', '');
  dicts[code] = dict;
}
const SUPPORTED = new Set(LANGS.map((l) => l.code));

const STORAGE_KEY = 'adbst.lang';

function readSetting(): LangSetting {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved && (saved === 'auto' || SUPPORTED.has(saved))) return saved;
  } catch {
    /* brak localStorage */
  }
  return 'auto';
}

function browserLocales(): string[] {
  try {
    return [...(navigator.languages ?? []), navigator.language].filter(Boolean);
  } catch {
    return [];
  }
}

/** locale systemu z backendu (Linux: LANGUAGE/LC_ALL/LC_MESSAGES/LANG, Windows: API) */
let systemLocales: string[] = [];

function resolve(setting: LangSetting): Lang {
  if (setting !== 'auto' && SUPPORTED.has(setting)) return setting;
  // WebKitGTK potrafi zwracać samo en-US - locale z systemu ma pierwszeństwo
  return matchLocale([...systemLocales, ...browserLocales()]);
}

/** Ustawienie użytkownika ('auto' albo kod języka). */
export const langSetting = writable<LangSetting>(readSetting());
/** Faktycznie używany język. */
export const lang = writable<Lang>(resolve(get(langSetting)));

lang.subscribe((l) => {
  try {
    document.documentElement.lang = l;
  } catch {
    /* prerender */
  }
});

/** Język wykryty dla trybu auto (reaktywnie - do podpisu w selektorze). */
export const autoLang = writable<Lang>(resolve('auto'));

/** Pobiera locale systemu z backendu i (w trybie auto) ustawia język. */
export async function initLang(): Promise<void> {
  try {
    systemLocales = await invoke<string[]>('system_locales');
  } catch {
    systemLocales = [];
  }
  autoLang.set(resolve('auto'));
  lang.set(resolve(get(langSetting)));
}

export function setLang(setting: LangSetting) {
  langSetting.set(setting);
  try {
    localStorage.setItem(STORAGE_KEY, setting);
  } catch {
    /* brak localStorage */
  }
  lang.set(resolve(setting));
}

/** Język wykryty dla trybu auto (do podpisu w selektorze). */
export function detectedLang(): Lang {
  return resolve('auto');
}

function lookup(code: string, key: string, fallback?: string): string {
  const v = dicts[code]?.[key];
  if (v) return v;
  return (en as Dict)[key] ?? fallback ?? key;
}

/** Reaktywna funkcja tłumacząca: $t('nav.pull'). */
export const t = derived(lang, ($lang) => (key: string, fallback?: string): string => lookup($lang, key, fallback));

/** Nie-reaktywne tłumaczenie (np. w logice JS). */
export function tr(key: string, fallback?: string): string {
  return lookup(get(lang), key, fallback);
}

/** Kod języka dla zasobów APK (resources.arsc używa 2-literowych kodów; indonezyjski = "in"). */
export function apkLang(l: Lang): string {
  const base = (l || FALLBACK).split('-')[0];
  return base === 'id' ? 'in' : base;
}
