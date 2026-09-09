/**
 * 组织身份（「我」在某组织内的昵称/头像）——插件 v1 桩：
 * 壳层 org-identity 依赖内核 identity 域组织作用域键（未进 SDK 面，A19 遗留），
 * v1 恒返回空身份（展示回退个人身份，与壳层空身份语义一致）。
 */
export type OrgIdentity = {
  nickname: string;
  avatar: string;
  /** 开启后在该组织内所有场景使用个人头像/昵称替代组织身份 */
  usePersonalIdentity: boolean;
};

const DEFAULT_IDENTITY: OrgIdentity = { nickname: '', avatar: '', usePersonalIdentity: false };

/** v1 恒空身份（壳层缺省值同形；展示层走个人身份回退） */
export function getOrgIdentity(_orgId: string): OrgIdentity {
  return { ...DEFAULT_IDENTITY };
}
