/**
 * 空间 key 约定（唯一定义）：个人空间固定 'personal'，组织空间 'org:<orgId>'。
 * mock/contacts（types.ts）与 mock/messages 均从这里 re-export，调用方 import 路径不变；
 * 新代码手写空间 key 前一律先走 spaceKeyOf。
 */
export function spaceKeyOf(space: { type: 'personal' } | { type: 'org'; orgId: string }): string {
  return space.type === 'org' ? `org:${space.orgId}` : 'personal';
}
