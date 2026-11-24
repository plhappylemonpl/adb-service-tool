#!/usr/bin/env node
// Test tłumaczeń GUI: każdy src/lib/locales/<kod>.json względem en.json
//  - komplet kluczy (zero brakujących, zero nadmiarowych, zero pustych),
//  - te same placeholdery interpolacji ({v}, {n}, ...),
//  - plik dla każdego języka z LANGS i brak plików spoza listy,
//  - dopasowanie locale systemu (nl-BE, fr-BE, cs-CZ, xx-YY -> en ...).
// Użycie: node scripts/check-locales.mjs   (kod wyjścia 1 przy błędach)
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { LANGS, matchLocale, placeholders } from '../src/lib/localeMatch.js';

const dir = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'lib', 'locales');
const load = (code) => JSON.parse(readFileSync(join(dir, `${code}.json`), 'utf8'));
const en = load('en');
const keys = Object.keys(en);
const errors = [];

const files = readdirSync(dir).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5));
for (const l of LANGS) if (!files.includes(l.code)) errors.push(`brak pliku ${l.code}.json`);
for (const f of files) if (!LANGS.some((l) => l.code === f)) errors.push(`${f}.json nie jest na liście LANGS`);

for (const { code } of LANGS) {
  if (!files.includes(code)) continue;
  let d;
  try {
    d = load(code);
  } catch (e) {
    errors.push(`${code}.json: nieprawidłowy JSON (${e.message})`);
    continue;
  }
  for (const k of keys) {
    if (!(k in d)) errors.push(`${code}: brak klucza ${k}`);
    else if (typeof d[k] !== 'string' || !d[k].trim()) errors.push(`${code}: pusta wartość ${k}`);
    else if (placeholders(d[k]).join() !== placeholders(en[k]).join())
      errors.push(`${code}: ${k} placeholdery ${placeholders(d[k])} != ${placeholders(en[k])}`);
  }
  for (const k of Object.keys(d)) if (!(k in en)) errors.push(`${code}: nadmiarowy klucz ${k}`);
}

const cases = [
  [['nl-BE'], 'nl-BE'], [['nl_BE.UTF-8'], 'nl-BE'], [['nl-NL'], 'nl'], [['fr-BE'], 'fr'], [['fr-CA'], 'fr'],
  [['de-AT'], 'de'], [['de-BE'], 'de'], [['cs-CZ'], 'cs'], [['pt-BR'], 'pt-BR'], [['pt-PT'], 'pt'], [['pt'], 'pt'],
  [['zh-CN'], 'zh-CN'], [['zh-Hans-CN'], 'zh-CN'], [['zh_TW'], 'en'], [['nb-NO'], 'nb'], [['nn-NO'], 'nb'],
  [['no'], 'nb'], [['in-ID'], 'id'], [['id_ID.UTF-8'], 'id'], [['sv-FI'], 'sv'], [['el-GR'], 'el'], [['ja-JP'], 'ja'],
  [['ko-KR'], 'ko'], [['xx-YY'], 'en'], [['C'], 'en'], [[], 'en'], [['xx-YY', 'pl-PL'], 'pl'],
  [['en-US', 'pl-PL'], 'en'], [['pl_PL.UTF-8@euro'], 'pl'], [['uk-UA'], 'uk'], [['sr-Latn-RS', 'de-CH'], 'de']
];
for (const [input, want] of cases) {
  const got = matchLocale(input);
  if (got !== want) errors.push(`matchLocale(${JSON.stringify(input)}) = ${got}, oczekiwano ${want}`);
}

if (errors.length) {
  console.error(errors.slice(0, 60).join('\n'));
  if (errors.length > 60) console.error(`... i ${errors.length - 60} więcej`);
  console.error(`\nBŁĘDY: ${errors.length}`);
  process.exit(1);
}
console.log(`OK: ${LANGS.length} języków × ${keys.length} kluczy, ${cases.length} przypadków dopasowania locale`);
