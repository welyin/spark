/**
 * 应用会话辅助（插件 v1 桩）：应用会话由壳层挂载区呈现（communication §4.2
 * 壳层保留面），插件列表适配层已过滤 `app:` 前缀会话——以下接口在 v1 模板
 * 分支中不会被触达，仅满足组件引用。
 */

/** 应用会话显示名：标题缺省回退 pluginId */
export function appConversationName(peerId: string, title: string): string {
  return title || peerId;
}

/** 应用会话屏蔽态：v1 恒 false（屏蔽为壳层本地状态） */
export function isAppConversationBlocked(_spaceKey: string, _pluginId: string): boolean {
  return false;
}

/** 切换屏蔽：v1 no-op */
export function toggleAppConversationBlocked(_spaceKey: string, _pluginId: string): void {}
