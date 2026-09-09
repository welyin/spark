/**
 * 头像/展示名入口（插件 v1 简化版）：
 * 壳层 avatar-sources 依赖通讯录/个人资料缓存（朋友备注 > 昵称 > 头像同步），
 * 插件 v1 无通讯录数据面消费（后续接 sdk.contacts 后可升级）——
 * 展示名恒用兜底值（会话标题 / 消息自报快照名），头像恒走自动头像
 * （UserAvatar 的 rootId 哈希渐变）。
 */

/** 对方头像源：v1 恒自动头像（image 为空串即走 UserAvatar 哈希渐变） */
export function personAvatarSource(_spaceKey: string, _rootId: string): { image: string } {
  return { image: '' };
}

/** 个人头像源（自己的消息）：v1 恒自动头像 */
export function personalAvatarSource(): { seed: string; name: string; image: string } {
  return { seed: '', name: '我', image: '' };
}

/** 统一展示名：v1 恒用兜底（会话标题/消息快照名） */
export function personDisplayName(_spaceKey: string, _rootId: string, fallback: string): string {
  return fallback;
}
