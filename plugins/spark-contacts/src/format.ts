/** 字节数人性化展示（1024 进制，保留 1 位小数） */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '-';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = -1;
  do {
    value /= 1024;
    unit += 1;
  } while (value >= 1024 && unit < units.length - 1);
  return `${value.toFixed(1)} ${units[unit]}`;
}

/** 长编码中间缩略（默认前 8…后 8），短文本原样返回 */
export function shortenMiddle(text: string, head = 8, tail = 8): string {
  if (!text || text.length <= head + tail + 3) return text;
  return `${text.slice(0, head)}…${text.slice(-tail)}`;
}
