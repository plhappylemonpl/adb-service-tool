<script lang="ts">
  // Lekki wykres liniowy SVG (bez bibliotek) - odświeżany na żywo.
  let {
    values,
    color = '#c9a249',
    height = 56,
    min = null,
    max = null
  }: { values: number[]; color?: string; height?: number; min?: number | null; max?: number | null } = $props();

  const W = 300;
  const geom = $derived.by(() => {
    const v = values.filter((x) => Number.isFinite(x));
    if (v.length < 2) return null;
    let lo = min ?? Math.min(...v);
    let hi = max ?? Math.max(...v);
    if (hi - lo < 1e-9) {
      hi += 1;
      lo -= 1;
    }
    const pad = (hi - lo) * 0.08;
    lo -= pad;
    hi += pad;
    const step = W / (v.length - 1);
    const pts = v.map((y, i) => [i * step, height - ((y - lo) / (hi - lo)) * height]);
    const line = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(' ');
    const area = `${line} L${W},${height} L0,${height} Z`;
    return { line, area, last: pts[pts.length - 1] };
  });
  const gid = `g${Math.random().toString(36).slice(2, 8)}`;
</script>

<svg viewBox="0 0 {W} {height}" preserveAspectRatio="none" class="block w-full" style="height:{height}px">
  <defs>
    <linearGradient id={gid} x1="0" x2="0" y1="0" y2="1">
      <stop offset="0%" stop-color={color} stop-opacity="0.35" />
      <stop offset="100%" stop-color={color} stop-opacity="0" />
    </linearGradient>
  </defs>
  {#if geom}
    <path d={geom.area} fill="url(#{gid})" />
    <path d={geom.line} fill="none" stroke={color} stroke-width="1.8" vector-effect="non-scaling-stroke" stroke-linejoin="round" />
    <circle cx={geom.last[0]} cy={geom.last[1]} r="3" fill={color} />
  {:else}
    <line x1="0" x2={W} y1={height / 2} y2={height / 2} stroke="#2d2d2d" stroke-dasharray="4 4" />
  {/if}
</svg>
