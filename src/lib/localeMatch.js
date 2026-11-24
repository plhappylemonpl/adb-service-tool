// Lista języków + dopasowanie locale systemu do obsługiwanego języka.
// Czysty JS (bez zależności) - używany przez i18n.ts i test scripts/check-locales.mjs.

/** @typedef {{ code: string, name: string, flag: string }} LangInfo */

/** @type {LangInfo[]} kolejność = kolejność w selektorze */
export const LANGS = [
  { code: 'en', name: 'English', flag: 'gb' },
  { code: 'pl', name: 'Polski', flag: 'pl' },
  { code: 'cs', name: 'Čeština', flag: 'cz' },
  { code: 'nl', name: 'Nederlands', flag: 'nl' },
  { code: 'nl-BE', name: 'Nederlands (België)', flag: 'be' },
  { code: 'fr', name: 'Français', flag: 'fr' },
  { code: 'de', name: 'Deutsch', flag: 'de' },
  { code: 'es', name: 'Español', flag: 'es' },
  { code: 'pt-BR', name: 'Português (Brasil)', flag: 'br' },
  { code: 'pt', name: 'Português', flag: 'pt' },
  { code: 'it', name: 'Italiano', flag: 'it' },
  { code: 'sk', name: 'Slovenčina', flag: 'sk' },
  { code: 'uk', name: 'Українська', flag: 'ua' },
  { code: 'ru', name: 'Русский', flag: 'ru' },
  { code: 'hu', name: 'Magyar', flag: 'hu' },
  { code: 'ro', name: 'Română', flag: 'ro' },
  { code: 'tr', name: 'Türkçe', flag: 'tr' },
  { code: 'sv', name: 'Svenska', flag: 'se' },
  { code: 'nb', name: 'Norsk', flag: 'no' },
  { code: 'da', name: 'Dansk', flag: 'dk' },
  { code: 'fi', name: 'Suomi', flag: 'fi' },
  { code: 'el', name: 'Ελληνικά', flag: 'gr' },
  { code: 'ja', name: '日本語', flag: 'jp' },
  { code: 'ko', name: '한국어', flag: 'kr' },
  { code: 'zh-CN', name: '简体中文', flag: 'cn' },
  { code: 'id', name: 'Bahasa Indonesia', flag: 'id' },
  { code: 'vi', name: 'Tiếng Việt', flag: 'vn' }
];

export const FALLBACK = 'en';

const BY_LOWER = new Map(LANGS.map((l) => [l.code.toLowerCase(), l.code]));

/** Języki bazowe, które mają osobną odmianę regionalną: baza -> kod bez regionu. */
const BASE_ALIASES = { no: 'nb', nn: 'nb', in: 'id', iw: null, zh: 'zh-CN' };

/**
 * Normalizuje tag locale: `pl_PL.UTF-8@euro` -> `pl-pl`, `zh-Hans-CN` -> `zh-cn`.
 * Zwraca null dla `C`, `POSIX` i pustych.
 * @param {string} raw
 */
export function normalizeLocale(raw) {
  if (!raw) return null;
  let s = String(raw).trim().split('.')[0].split('@')[0].replace(/_/g, '-').toLowerCase();
  if (!s || s === 'c' || s === 'posix') return null;
  const parts = s.split('-').filter(Boolean);
  if (parts[0] === 'zh') {
    // chiński tradycyjny (Hant / TW / HK / MO) - nieobsługiwany
    if (parts.some((p) => ['hant', 'tw', 'hk', 'mo'].includes(p))) return 'zh-hant';
    return 'zh-cn';
  }
  // usuń skrypt (np. sr-Latn-RS -> sr-rs)
  const region = parts.slice(1).find((p) => /^[a-z]{2}$|^\d{3}$/.test(p));
  return region ? `${parts[0]}-${region}` : parts[0];
}

/**
 * Dopasowuje listę locale (w kolejności preferencji) do obsługiwanego języka.
 * Dla każdego kandydata: dokładne dopasowanie (`nl-BE`), potem język bazowy (`fr-CA` -> `fr`).
 * Brak dopasowania -> `en`.
 * @param {readonly string[]} candidates
 * @returns {string}
 */
export function matchLocale(candidates) {
  for (const raw of candidates ?? []) {
    const n = normalizeLocale(raw);
    if (!n || n === 'zh-hant') continue;
    const exact = BY_LOWER.get(n);
    if (exact) return exact;
    const base = n.split('-')[0];
    if (base in BASE_ALIASES) {
      const a = BASE_ALIASES[/** @type {keyof typeof BASE_ALIASES} */ (base)];
      if (a) return a;
      continue;
    }
    const b = BY_LOWER.get(base);
    if (b) return b;
  }
  return FALLBACK;
}

/** Placeholdery interpolacji w tekście: `{v}`, `{count}`. @param {string} s */
export function placeholders(s) {
  return [...String(s).matchAll(/\{[A-Za-z0-9_]+\}/g)].map((m) => m[0]).sort();
}
