/**
 * 提取 IPC 调用失败的用户可读消息。
 * Electron invoke  reject 的格式为 "Error invoking remote method 'channel': Error: 真实消息"，
 * 剥离包装只保留主进程抛出的原始消息。
 */
export function errorMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  const match = raw.match(/Error invoking remote method '[^']+':\s*(?:Error:\s*)?(.+)$/s);
  return (match?.[1] ?? raw).trim();
}
