/**
 * 身份备份完成标记（localStorage，按 rootId 维度）。
 * 标记在任一备份动作完成时写入：注册页复制助记词 / 查看备份二维码 / 密码查看助记词。
 * 未标记时"我的"页面展示"未备份"提醒。
 */

const KEY_PREFIX = 'spark:identity-backed-up:';

export function isIdentityBackupMarked(rootId: string | null): boolean {
  if (!rootId) {
    return true; // 无身份时不展示提醒
  }
  try {
    return localStorage.getItem(KEY_PREFIX + rootId) === '1';
  } catch {
    return true; // 存储不可用时静默，不打扰
  }
}

export function markIdentityBackupDone(rootId: string): void {
  try {
    localStorage.setItem(KEY_PREFIX + rootId, '1');
  } catch {
    // 存储不可用忽略
  }
}
