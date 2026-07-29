// 组织页各组件共享的类型定义（.vue 经 shim 只能默认导入，类型需放在 .ts 中）
import type { OrgView } from '../../api';

export type OrganizationView = OrgView;

export type OrganizationMember = OrgView['members'][number];

// PluginCatalogItem 已 DTO 化到 api/types.ts（plugin.listCatalog 的返回类型），此处 re-export 兼容
export type { PluginCatalogItem } from '../../api/types';

export type CreateForm = {
  name: string;
  description: string;
  /** 组织 logo（dataURL）；空串表示未上传，展示时按 orgId 自动生成头像 */
  avatar: string;
};
