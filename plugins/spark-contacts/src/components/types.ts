// 通讯录各组件共享的类型定义（.vue 经 shim 只能默认导入，类型需放在 .ts，同 org/types.ts 约定）

/** 列表/面板共用的联系人视图：个人空间由 mock 朋友构造，组织空间由真实成员 + 本地资料合成 */
export type ContactItem = {
  rootId: string;
  /** 展示名（备注名优先，ui-contacts §2.2/§5.4） */
  displayName: string;
  /** 列表第二行：签名（无签名则不显示该行；RootID 属隐私不上列表） */
  subtitle: string;
  /** 头像配色种子：个人=rootId；组织身份=rootId@orgId（与个人配色区分）。缺省=rootId */
  avatarSeed?: string;
  /** 已上传的头像图片（dataURL）；空/缺省=自动配色头像 */
  avatarImage?: string;
  /** 对方签名（个人=朋友签名；组织=组织身份签名 mock；空则详情页隐藏该行） */
  signature?: string;
  /** 对方性别（详情页昵称旁图标，无则不显示） */
  gender?: 'male' | 'female';
  /** 对方昵称/组织身份昵称（设置备注名后详情页单独展示） */
  nickname?: string;
  blocked: boolean;
  isSelf: boolean;
  /** 组织空间真实角色（§3.2/§9.5 管理员/成员标签） */
  role?: 'admin' | 'member';
  /** 组织空间真实加入时间 */
  joinedAt?: number;
};

/** 资料卡「分组」下拉选项（个人=扁平；组织=树扁平化按深度缩进）；'' = 未分组 */
export type GroupOption = { id: string; label: string };

/** 通讯录右栏内容态：联系人（默认，分组->成员->资料卡）/ 新的朋友 / 标签 */
export type RightView = 'contact' | 'new-friends' | 'tags';
