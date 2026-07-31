/**
 * HTTP 代理设置纯逻辑（ProxySettings.vue 的表单值 ↔ 后端 "host:port" 互转）。
 *
 * 格式校验在 Rust 侧（src-tauri proxy.rs validate_proxy）统一把关，
 * 前端只做拆分/拼接与最基本的非空判断，不重复实现校验规则。
 */

/** 拆分后端返回的 "host:port"（兼容 [IPv6]:port）→ 表单字段；无法拆分返回 null。 */
export function splitProxy(proxy: string | null): { host: string; port: string } | null {
  if (!proxy) {
    return null;
  }
  const trimmed = proxy.trim();
  // [IPv6]:port —— 端口取最后一个 ':' 之后，主机含方括号保留原样回显
  const bracket = trimmed.match(/^\[(.+)\]:(\d+)$/);
  if (bracket) {
    return { host: `[${bracket[1]}]`, port: bracket[2] };
  }
  const idx = trimmed.lastIndexOf(':');
  if (idx <= 0 || idx === trimmed.length - 1) {
    return null;
  }
  return { host: trimmed.slice(0, idx), port: trimmed.slice(idx + 1) };
}

/** 拼接表单字段 → 后端入参 "host:port"；任一为空返回 ''（=关闭）。 */
export function joinProxy(host: string, port: string): string {
  const h = host.trim();
  const p = port.trim();
  if (!h || !p) {
    return '';
  }
  return `${h}:${p}`;
}

/** 端口输入框的即时约束：纯数字且 1-65535（完整校验仍在保存时由后端把关）。 */
export function isPortPlausible(port: string): boolean {
  if (!/^\d+$/.test(port.trim())) {
    return false;
  }
  const value = Number(port.trim());
  return value >= 1 && value <= 65535;
}
