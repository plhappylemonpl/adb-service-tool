// Formatowanie wartości do GUI.

export function humanBytes(b: number | null | undefined): string {
  if (b == null || !isFinite(b)) return '—';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = b;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${b} B` : `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`;
}

export function humanKb(kb: number | null | undefined): string {
  return kb == null ? '—' : humanBytes(kb * 1024);
}

export function humanDuration(sec: number | null | undefined): string {
  if (sec == null || !isFinite(sec) || sec < 0) return '—';
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  if (h) return `${h}h ${m.toString().padStart(2, '0')}m`;
  if (m) return `${m}m ${r.toString().padStart(2, '0')}s`;
  return `${r}s`;
}

export function basename(p: string): string {
  return p.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || p;
}

export function errText(e: unknown): string {
  return typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
