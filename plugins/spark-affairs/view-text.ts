/**
 * 详情视图共享展示助手（纯函数）。
 * 时间与身份 id 仅为本机展示口径：时刻是签名者声明值或本副本锚定值，
 * id 截短不用于任何判定（判定一律全量哈希/身份原文）。
 */
import type { LadderLevel } from './model';

export function shortId(identity: string): string {
  return identity.length > 16 ? `${identity.slice(0, 12)}…` : identity;
}

export function formatDate(ts: number): string {
  return new Date(ts).toLocaleString();
}

export function levelText(level: LadderLevel): string {
  return { observer: '观察者', contributor: '贡献者', voter: '投票者' }[level];
}
