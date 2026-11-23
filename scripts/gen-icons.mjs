#!/usr/bin/env node
// Generator ikony aplikacji: złota litera "D" na czarnym tle (motyw Drapco, #c9a249).
// Bez zależności - czysty Node (zlib). Wynik: app-icon.png (1024x1024),
// z którego `npx tauri icon app-icon.png` tworzy komplet ikon w src-tauri/icons/.
//
// Użycie:  node scripts/gen-icons.mjs [plik_wyjściowy.png]
import { deflateSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';

const SIZE = 1024;
const SS = 4; // supersampling 4x4 (antyaliasing)
const out = process.argv[2] ?? 'app-icon.png';

const GOLD_TOP = [0xe0, 0xbf, 0x72];
const GOLD_MID = [0xc9, 0xa2, 0x49];
const GOLD_BOT = [0x8a, 0x70, 0x30];
const BG = [0x0a, 0x0a, 0x0a];

const inRoundRect = (x, y, x0, y0, x1, y1, r) => {
  if (x < x0 || x > x1 || y < y0 || y > y1) return false;
  const cx = Math.min(Math.max(x, x0 + r), x1 - r);
  const cy = Math.min(Math.max(y, y0 + r), y1 - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
};

// Litera "D": prostokąt + półelipsa, z wyciętym środkiem
const inD = (x, y) => {
  const outer =
    (x >= 0.27 && x <= 0.5 && y >= 0.22 && y <= 0.78) ||
    (x >= 0.5 && ((x - 0.5) / 0.25) ** 2 + ((y - 0.5) / 0.28) ** 2 <= 1);
  const inner =
    (x >= 0.395 && x <= 0.5 && y >= 0.335 && y <= 0.665) ||
    (x >= 0.5 && ((x - 0.5) / 0.135) ** 2 + ((y - 0.5) / 0.165) ** 2 <= 1);
  return outer && !inner;
};

const lerp = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);
const goldAt = (y) => (y < 0.5 ? lerp(GOLD_TOP, GOLD_MID, y / 0.5) : lerp(GOLD_MID, GOLD_BOT, (y - 0.5) / 0.5));

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);
for (let py = 0; py < SIZE; py++) {
  raw[py * (SIZE * 4 + 1)] = 0; // filtr: none
  for (let px = 0; px < SIZE; px++) {
    let r = 0, g = 0, b = 0, a = 0;
    for (let sy = 0; sy < SS; sy++) {
      for (let sx = 0; sx < SS; sx++) {
        const x = (px + (sx + 0.5) / SS) / SIZE;
        const y = (py + (sy + 0.5) / SS) / SIZE;
        if (!inRoundRect(x, y, 0.02, 0.02, 0.98, 0.98, 0.2)) continue;
        let c = BG;
        // cienka złota ramka
        if (!inRoundRect(x, y, 0.045, 0.045, 0.955, 0.955, 0.18)) c = GOLD_BOT;
        else if (inD(x, y)) c = goldAt(y);
        r += c[0]; g += c[1]; b += c[2]; a += 255;
      }
    }
    const n = SS * SS;
    const o = py * (SIZE * 4 + 1) + 1 + px * 4;
    const cov = a / n;
    // kolor "premultiplied" -> zwykły
    raw[o] = cov ? Math.round((r / n) * 255 / cov) : 0;
    raw[o + 1] = cov ? Math.round((g / n) * 255 / cov) : 0;
    raw[o + 2] = cov ? Math.round((b / n) * 255 / cov) : 0;
    raw[o + 3] = Math.round(cov);
  }
}

// --- minimalny koder PNG ---
const crcTable = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const byte of buf) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
};
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk('IHDR', ihdr),
  chunk('IDAT', deflateSync(raw, { level: 9 })),
  chunk('IEND', Buffer.alloc(0))
]);
writeFileSync(out, png);
console.log(`OK: ${out} (${SIZE}x${SIZE})`);
