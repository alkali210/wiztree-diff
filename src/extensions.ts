const palette = [
  [70, 124, 227],
  [152, 72, 209],
  [35, 166, 124],
  [207, 126, 26],
  [226, 68, 88],
  [173, 173, 29],
  [30, 163, 202],
  [191, 72, 162],
  [86, 166, 75],
  [102, 90, 205],
  [206, 106, 44],
  [45, 153, 160],
  [193, 68, 55],
  [105, 141, 30],
  [125, 125, 139],
  [41, 114, 180],
] as const;
export function extensionColor(extension: string): string {
  let hash = 2166136261;
  for (const byte of new TextEncoder().encode(extension))
    hash = Math.imul(hash ^ byte, 16777619) >>> 0;
  const c = palette[hash & 15];
  return "rgb(" + c.join(",") + ")";
}
export function extensionLabel(extension: string): string {
  return extension || "(无扩展名)";
}
