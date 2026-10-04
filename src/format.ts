export function bytes(value: string | bigint | null | undefined): string {
  if (value == null) return "—";
  const n = BigInt(value),
    a = n < 0n ? -n : n;
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
  let i = 0,
    d = 1n;
  while (a >= d * 1024n && i < units.length - 1) {
    d *= 1024n;
    i++;
  }
  const v = (a * 100n) / d;
  return (
    (n < 0n ? "-" : "") +
    (v / 100n).toString() +
    (i ? "." + (v % 100n).toString().padStart(2, "0") : "") +
    " " +
    units[i]
  );
}
export function signed(value: string): string {
  return (BigInt(value) > 0n ? "+" : "") + bytes(value);
}
export function errorText(e: unknown): string {
  if (typeof e !== "object" || e === null || !("message" in e))
    return String(e);
  const parts = [String(e.message)];
  if ("code" in e && e.code) parts.unshift("[" + String(e.code) + "]");
  if ("source" in e && e.source) parts.push("来源：" + String(e.source));
  if ("record" in e && e.record != null)
    parts.push("CSV 记录 " + String(e.record));
  if ("column" in e && e.column) parts.push("字段：" + String(e.column));
  return parts.join(" · ");
}
