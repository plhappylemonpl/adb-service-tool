#!/usr/bin/env node
// Grafiki instalatora NSIS (motyw Drapco: głęboka czerń + złoto #c9a249).
// Bez zależności - czysty Node. NSIS/MUI2 wymaga BMP 24-bit (bez kanału alfa):
//   installer-sidebar.bmp  164x314  - pasek boczny strony powitalnej / końcowej
//   installer-header.bmp   150x57   - nagłówek pozostałych stron
//
// Użycie:  node scripts/gen-installer-art.mjs [katalog_wyjściowy]   (domyślnie src-tauri/windows)
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const outDir = process.argv[2] ?? 'src-tauri/windows';
const SS = 4; // supersampling 4x4

const GOLD = [0xc9, 0xa2, 0x49];
const GOLD_HI = [0xe6, 0xc5, 0x78];
const GOLD_LO = [0x8a, 0x70, 0x30];
const GOLD_MUTED = [0xa0, 0x84, 0x45];
const LOGO_BG = [0x0a, 0x0a, 0x0a];

const lerp = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);
const clamp01 = (v) => Math.max(0, Math.min(1, v));

// --- kształty logo (jak scripts/gen-icons.mjs, współrzędne 0..1) ---
const inRoundRect = (x, y, x0, y0, x1, y1, r) => {
  if (x < x0 || x > x1 || y < y0 || y > y1) return false;
  const cx = Math.min(Math.max(x, x0 + r), x1 - r);
  const cy = Math.min(Math.max(y, y0 + r), y1 - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
};
const inD = (x, y) => {
  const outer =
    (x >= 0.27 && x <= 0.5 && y >= 0.22 && y <= 0.78) ||
    (x >= 0.5 && ((x - 0.5) / 0.25) ** 2 + ((y - 0.5) / 0.28) ** 2 <= 1);
  const inner =
    (x >= 0.395 && x <= 0.5 && y >= 0.335 && y <= 0.665) ||
    (x >= 0.5 && ((x - 0.5) / 0.135) ** 2 + ((y - 0.5) / 0.165) ** 2 <= 1);
  return outer && !inner;
};
const goldAt = (y) => (y < 0.5 ? lerp(GOLD_HI, GOLD, y / 0.5) : lerp(GOLD, GOLD_LO, (y - 0.5) / 0.5));

/** Kolor logo w punkcie (u,v) w kwadracie logo albo null (poza logo). */
const logoSample = (u, v) => {
  if (!inRoundRect(u, v, 0.02, 0.02, 0.98, 0.98, 0.2)) return null;
  if (!inRoundRect(u, v, 0.055, 0.055, 0.945, 0.945, 0.17)) return GOLD_LO;
  if (inD(u, v)) return goldAt(v);
  return LOGO_BG;
};

// --- prosta czcionka kreskowa (wersaliki potrzebne w napisach) ---
const arc = (cx, cy, rx, ry, a0, a1, n = 18) => {
  const pts = [];
  for (let i = 0; i <= n; i++) {
    const t = ((a0 + ((a1 - a0) * i) / n) * Math.PI) / 180;
    pts.push([cx + rx * Math.cos(t), cy + ry * Math.sin(t)]);
  }
  return pts;
};
// glif: { w: szerokość (wysokość = 1), p: lista łamanych }
const GLYPHS = {
  A: { w: 0.78, p: [[[0, 1], [0.39, 0], [0.78, 1]], [[0.15, 0.64], [0.63, 0.64]]] },
  B: {
    w: 0.66,
    p: [
      [[0, 0], [0, 1]],
      [[0, 0], [0.38, 0], ...arc(0.38, 0.24, 0.24, 0.24, -90, 90), [0, 0.48]],
      [[0, 0.48], [0.4, 0.48], ...arc(0.4, 0.74, 0.26, 0.26, -90, 90), [0, 1]]
    ]
  },
  C: { w: 0.72, p: [arc(0.5, 0.5, 0.5, 0.5, 40, 320, 28)] },
  D: { w: 0.7, p: [[[0, 0], [0, 1]], [[0, 0], [0.25, 0], ...arc(0.25, 0.5, 0.45, 0.5, -90, 90, 24), [0, 1]]] },
  E: { w: 0.58, p: [[[0.58, 0], [0, 0], [0, 1], [0.58, 1]], [[0, 0.5], [0.48, 0.5]]] },
  I: { w: 0, p: [[[0, 0], [0, 1]]] },
  L: { w: 0.55, p: [[[0, 0], [0, 1], [0.55, 1]]] },
  O: { w: 0.86, p: [arc(0.43, 0.5, 0.43, 0.5, 0, 360, 32)] },
  P: { w: 0.62, p: [[[0, 1], [0, 0], [0.34, 0], ...arc(0.34, 0.27, 0.28, 0.27, -90, 90), [0, 0.54]]] },
  R: {
    w: 0.64,
    p: [[[0, 1], [0, 0], [0.34, 0], ...arc(0.34, 0.27, 0.28, 0.27, -90, 90), [0, 0.54]], [[0.3, 0.54], [0.64, 1]]]
  },
  S: {
    w: 0.62,
    p: [
      [
        ...arc(0.31, 0.25, 0.3, 0.25, -20, -180, 12),
        ...arc(0.31, 0.25, 0.3, 0.25, 180, 90, 8),
        ...arc(0.31, 0.75, 0.31, 0.25, -90, 0, 8),
        ...arc(0.31, 0.75, 0.31, 0.25, 0, 160, 12)
      ]
    ]
  },
  T: { w: 0.7, p: [[[0, 0], [0.7, 0]], [[0.35, 0], [0.35, 1]]] },
  V: { w: 0.76, p: [[[0, 0], [0.38, 1], [0.76, 0]]] },
  ' ': { w: 0.45, p: [] }
};

/** Odcinki napisu w pikselach: x,y = lewy górny róg, h = wysokość liter. */
function textSegments(str, x, y, h, spacing, align = 'left') {
  const glyphs = [...str].map((c) => GLYPHS[c] ?? GLYPHS[' ']);
  const width = glyphs.reduce((s, g, i) => s + g.w * h + (i ? spacing : 0), 0);
  let cx = align === 'center' ? x - width / 2 : x;
  const segs = [];
  for (const g of glyphs) {
    for (const line of g.p) {
      for (let i = 1; i < line.length; i++) {
        segs.push([cx + line[i - 1][0] * h, y + line[i - 1][1] * h, cx + line[i][0] * h, y + line[i][1] * h]);
      }
    }
    cx += g.w * h + spacing;
  }
  return segs;
}

const segDist2 = (px, py, [x1, y1, x2, y2]) => {
  const dx = x2 - x1, dy = y2 - y1;
  const len2 = dx * dx + dy * dy;
  const t = len2 ? clamp01(((px - x1) * dx + (py - y1) * dy) / len2) : 0;
  const qx = x1 + t * dx - px, qy = y1 + t * dy - py;
  return qx * qx + qy * qy;
};

/** Warstwa napisu: { segs, half (pół grubości kreski), color(x,y) }. */
const textLayer = (segs, stroke, color) => {
  const half = stroke / 2;
  const box = segs.map(([x1, y1, x2, y2]) => [
    Math.min(x1, x2) - half, Math.min(y1, y2) - half, Math.max(x1, x2) + half, Math.max(y1, y2) + half
  ]);
  return {
    hit(x, y) {
      for (let i = 0; i < segs.length; i++) {
        const b = box[i];
        if (x < b[0] || x > b[2] || y < b[1] || y > b[3]) continue;
        if (segDist2(x, y, segs[i]) <= half * half) return true;
      }
      return false;
    },
    color
  };
};

/**
 * Renderuje obraz W x H: tło bg(x,y) + logo (kwadrat) + warstwy napisów + extra(x,y,c) (linie, akcenty).
 * Zwraca bufor RGB (wiersze od góry).
 */
function render(W, H, { bg, logo, texts = [], extra }) {
  const rgb = Buffer.alloc(W * H * 3);
  for (let py = 0; py < H; py++) {
    for (let px = 0; px < W; px++) {
      let r = 0, g = 0, b = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const x = px + (sx + 0.5) / SS;
          const y = py + (sy + 0.5) / SS;
          let c = bg(x, y);
          if (extra) c = extra(x, y, c);
          if (logo) {
            const u = (x - logo.x) / logo.size, v = (y - logo.y) / logo.size;
            if (u >= 0 && u <= 1 && v >= 0 && v <= 1) c = logoSample(u, v) ?? c;
          }
          for (const t of texts) if (t.hit(x, y)) c = typeof t.color === 'function' ? t.color(x, y) : t.color;
          r += c[0]; g += c[1]; b += c[2];
        }
      }
      const n = SS * SS, o = (py * W + px) * 3;
      rgb[o] = Math.round(r / n); rgb[o + 1] = Math.round(g / n); rgb[o + 2] = Math.round(b / n);
    }
  }
  return rgb;
}

/** BMP 24-bit (BITMAPINFOHEADER, wiersze od dołu, BGR, wyrównanie do 4 B). */
function bmp24(W, H, rgb) {
  const row = Math.ceil((W * 3) / 4) * 4;
  const size = 54 + row * H;
  const buf = Buffer.alloc(size);
  buf.write('BM', 0, 'ascii');
  buf.writeUInt32LE(size, 2);
  buf.writeUInt32LE(54, 10);
  buf.writeUInt32LE(40, 14);
  buf.writeInt32LE(W, 18);
  buf.writeInt32LE(H, 22);
  buf.writeUInt16LE(1, 26);
  buf.writeUInt16LE(24, 28);
  buf.writeUInt32LE(0, 30); // BI_RGB
  buf.writeUInt32LE(row * H, 34);
  buf.writeInt32LE(2835, 38); // 72 DPI
  buf.writeInt32LE(2835, 42);
  for (let y = 0; y < H; y++) {
    const dst = 54 + (H - 1 - y) * row;
    for (let x = 0; x < W; x++) {
      const s = (y * W + x) * 3;
      buf[dst + x * 3] = rgb[s + 2];
      buf[dst + x * 3 + 1] = rgb[s + 1];
      buf[dst + x * 3 + 2] = rgb[s];
    }
  }
  return buf;
}

const glow = (x, y, cx, cy, r, strength) => {
  const d = Math.hypot(x - cx, y - cy) / r;
  return d >= 1 ? 0 : strength * (1 - d) ** 2;
};
const goldText = (y0, h) => (x, y) => goldAt(clamp01((y - y0) / h));

// --- pasek boczny 164x314 ---
function sidebar() {
  const W = 164, H = 314, cx = W / 2;
  const logo = { x: cx - 44, y: 62, size: 88 };
  const texts = [
    textLayer(textSegments('DRAPCO', cx, 172, 14, 5.5, 'center'), 1.9, goldText(172, 14)),
    textLayer(textSegments('ADB SERVICE TOOL', cx, 207, 7, 2.6, 'center'), 1.05, GOLD_MUTED)
  ];
  const bg = (x, y) => {
    const base = lerp([0, 0, 0], [8, 7, 4], y / H);
    const gl = glow(x, y, cx, 106, 92, 0.16) + glow(x, y, cx, H + 30, 120, 0.07);
    return lerp(base, GOLD, gl);
  };
  const extra = (x, y, c) => {
    // linia pod nazwą (zanikająca ku krawędziom)
    if (y >= 195 && y < 196 && Math.abs(x - cx) < 40) return lerp(c, GOLD, 1 - Math.abs(x - cx) / 40);
    // złota krawędź po prawej, wygaszana na końcach
    if (x >= W - 1) return lerp(c, GOLD_LO, Math.sin((Math.PI * y) / H) * 0.9);
    // narożniki ramki
    const corner = (x0, y0, sx, sy) =>
      (Math.abs(y - y0) < 0.75 && (x - x0) * sx >= 0 && (x - x0) * sx < 16) ||
      (Math.abs(x - x0) < 0.75 && (y - y0) * sy >= 0 && (y - y0) * sy < 16);
    if (corner(10, 10, 1, 1) || corner(W - 11, H - 11, -1, -1)) return lerp(c, GOLD, 0.55);
    return c;
  };
  return { W, H, rgb: render(W, H, { bg, logo, texts, extra }) };
}

// --- nagłówek 150x57 ---
function header() {
  const W = 150, H = 57;
  const logo = { x: 7, y: 8, size: 41 };
  const texts = [
    textLayer(textSegments('DRAPCO', 58, 15, 11, 4.2), 1.6, goldText(15, 11)),
    textLayer(textSegments('ADB SERVICE TOOL', 58, 35, 5.6, 2.1), 0.95, GOLD_MUTED)
  ];
  const bg = (x, y) => lerp([0, 0, 0], GOLD, glow(x, y, 27, 28, 40, 0.1));
  const extra = (x, y, c) => {
    if (y >= 30 && y < 31 && x >= 58 && x < 140) return lerp(c, GOLD, 1 - (x - 58) / 82);
    return c;
  };
  return { W, H, rgb: render(W, H, { bg, logo, texts, extra }) };
}

mkdirSync(outDir, { recursive: true });
for (const [name, img] of [['installer-sidebar.bmp', sidebar()], ['installer-header.bmp', header()]]) {
  const p = join(outDir, name);
  writeFileSync(p, bmp24(img.W, img.H, img.rgb));
  console.log(`${p} (${img.W}x${img.H})`);
}
