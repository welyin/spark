/**
 * 简单语义化版本比较（x.y.z / x.y；容忍 "v" 前缀与预发布/构建段）。
 * 用于设备管理「可更新」提示：a > b → 1，相等 → 0，a < b → -1。
 * 任一参数无法解析时返回 0（不判定，避免误报）。
 */
export function compareVersions(a: string, b: string): number {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (!pa || !pb) {
    return 0;
  }
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i++) {
    const x = pa[i] ?? 0;
    const y = pb[i] ?? 0;
    if (x !== y) {
      return x > y ? 1 : -1;
    }
  }
  return 0;
}

/** 解析版本号主体为数字段数组（"v" 前缀与预发布/构建段剥除）；非法返回 null。 */
function parseVersion(v: string): number[] | null {
  const core = v.trim().replace(/^[vV]/, '').split('-')[0].split('+')[0];
  const parts = core.split('.');
  if (parts.some((p) => !/^\d+$/.test(p))) {
    return null;
  }
  return parts.map((p) => Number(p));
}
