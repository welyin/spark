/**
 * 通讯录查询（插件 v1 桩）：壳层 friendOf 依赖通讯录缓存（判定「联系人
 * 已删除」）；插件 v1 无通讯录数据面消费（后续接 sdk.contacts 可升级）——
 * 恒返回 undefined 的**保守口径**：不误判「已删除」（v1 不禁止发送）。
 *
 * 注意：这与壳层「已删除联系人禁止发送」的语义有差异（v1 限制，已记录在
 * 迁移文档）——永远不会把正常联系人误判为已删除。
 */
export interface FriendRef {
  rootId: string;
}

export function friendOf(_spaceKey: string, _rootId: string): FriendRef | undefined {
  return undefined;
}
