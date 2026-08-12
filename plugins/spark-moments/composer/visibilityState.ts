/**
 * 朋友圈插件（spark-moments）· 可见性选择共享状态（composer ↔ visibility picker ↔ recipient editor）。
 *
 * 单例模块态：发动态页、四选一页、名单编辑器页经模块级可变状态共享「当前动态」的
 * 可见性 scope 与勾选名单（联系人/分组/标签），避免经 props 层层穿透 / 复杂事件。
 * 会话级记忆：scope 默认「公开」，本次会话内记忆上次选择（UI 设计 composer.md §4.1）。
 */

import { reactive } from 'vue';
import type { MomentsVisibleScope } from '../model';

export type RecipientSelection = {
  contactRootIds: string[];
  groupIds: string[];
  tagIds: string[];
};

/** 共享可见性状态（单例；会话内记忆） */
export const visibilityState = reactive<{
  scope: MomentsVisibleScope;
  selection: RecipientSelection;
}>({
  scope: 'all',
  selection: { contactRootIds: [], groupIds: [], tagIds: [] }
});

/** 重置（发动态页进入时可选调用） */
export function resetVisibilityState(scope: MomentsVisibleScope = 'all'): void {
  visibilityState.scope = scope;
  visibilityState.selection = { contactRootIds: [], groupIds: [], tagIds: [] };
}
