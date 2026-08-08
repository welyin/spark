/**
 * 空间级提醒门面（无数量）：某空间有未读消息或未读朋友/成员申请即视为
 * 「有新消息」。供顶部空间切换器（触发器红点 + 菜单行红点）等外壳 UI 使用；
 * 在 computed/渲染中调用即可保持响应式（底层 store 均为响应式缓存，首次
 * 访问触发该空间水合）。
 */
import { requestBadgeCount } from '../mock/contacts';
import { hasUnreadMessages } from './messages';

/** 该空间是否有未读提醒：未读消息（免打扰不计）或「新的朋友/成员」未读条目 */
export function hasSpaceNotification(spaceKey: string): boolean {
  return hasUnreadMessages(spaceKey) || requestBadgeCount(spaceKey) > 0;
}
