/**
 * 样例库包入口：纯函数，无数据域、无宿主依赖。
 * 被 sample-app 以构建期依赖方式打进其 bundle（vendor 锚定 + 哈希锁定）。
 */
export function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '-';
  }
  const units = ['B', 'KiB', 'MiB', 'GiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}
