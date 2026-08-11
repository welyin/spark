/**
 * N 天未使用自动锁定（device-trust-and-biometric 落地路线 §5）的本机设置存储。
 *
 * 存储契约（localStorage）：
 * - `spark.settings.autoLockDays`：number，默认 0 = 关闭；>0 表示 N 天未使用自动锁定
 * - `spark.settings.lastActiveAt`：时间戳 ms，最近一次成功登录/解锁时间
 *
 * 本机设置，不做多设备同步（设置项现状均为本机）。default 0（关闭）是
 * §1 已确认决策：默认长期登录，不默认踢人。
 */

const AUTO_LOCK_DAYS_KEY = 'spark.settings.autoLockDays';
const LAST_ACTIVE_AT_KEY = 'spark.settings.lastActiveAt';

/** 可选档位（天）；0 = 关闭 */
export const AUTO_LOCK_OPTIONS = [0, 7, 30, 90] as const;

function readInt(key: string, fallback: number): number {
  const raw = localStorage.getItem(key);
  if (raw === null) {
    return fallback;
  }
  const value = Number(raw);
  return Number.isFinite(value) ? value : fallback;
}

/** 当前自动锁定天数（0 = 关闭） */
export function getAutoLockDays(): number {
  return readInt(AUTO_LOCK_DAYS_KEY, 0);
}

/** 设置自动锁定天数（0 = 关闭） */
export function setAutoLockDays(days: number): void {
  localStorage.setItem(AUTO_LOCK_DAYS_KEY, String(days));
}

/** 最近一次成功登录/解锁时间戳（ms）；从未记录返回 null */
export function getLastActiveAt(): number | null {
  const raw = localStorage.getItem(LAST_ACTIVE_AT_KEY);
  if (raw === null) {
    return null;
  }
  const value = Number(raw);
  return Number.isFinite(value) && value > 0 ? value : null;
}

/** 登录/解锁成功时刷新最近活跃时间 */
export function touchLastActiveAt(now: number = Date.now()): void {
  localStorage.setItem(LAST_ACTIVE_AT_KEY, String(now));
}

/** 是否已超时：autoLockDays>0 且 now - lastActiveAt 超过 N 天 */
export function isAutoLockExpired(now: number = Date.now()): boolean {
  const days = getAutoLockDays();
  if (days <= 0) {
    return false;
  }
  const lastActive = getLastActiveAt();
  if (lastActive === null) {
    // 无活跃记录（首次装新版）：不强制锁定，视为本机刚启用
    return false;
  }
  // 时钟回拨取舍：若系统时间回拨使 now < lastActiveAt，elapsedDays 为负，
  // 必然 < days 不触发锁定（朝"更宽松"方向）——避免误把用户锁出去，取舍可接受。
  const elapsedDays = (now - lastActive) / (24 * 60 * 60 * 1000);
  return elapsedDays >= days;
}
