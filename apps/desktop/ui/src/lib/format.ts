const KB = 1024;
const MB = KB * 1024;
const GB = MB * 1024;

/** "48.2 MB"-style sizes, matching the engine's binary units. */
export function bytes(b: number): string {
  const trim = (s: string) => s.replace(/\.0+$/, "");
  if (b >= GB) return `${trim((b / GB).toFixed(2))} GB`;
  if (b >= 100 * MB) return `${Math.round(b / MB)} MB`;
  if (b >= MB) return `${trim((b / MB).toFixed(1))} MB`;
  return `${Math.round(b / KB)} KB`;
}

export const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;
