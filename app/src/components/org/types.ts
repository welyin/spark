// 组织页各组件共享的类型定义（.vue 经 shim 只能默认导入，类型需放在 .ts 中）
import type { OrgView } from '../../api';

export type OrganizationView = OrgView;

export type OrganizationMember = OrgView['members'][number];

// PluginCatalogItem 已 DTO 化到 api/types.ts（市场列表条目的 catalog 部分），此处 re-export 兼容
export type { PluginCatalogItem } from '../../api/types';

export type CreateForm = {
  name: string;
  description: string;
  /** 组织 logo（dataURL）；空串表示未上传，展示时按 orgId 自动生成头像 */
  avatar: string;
  /** 域类型（org-genesis §3.1）：leaf 普通组织（默认）/ community 共同体域（成员为组织）；创建后不可变更 */
  domainType: 'leaf' | 'community';
};
