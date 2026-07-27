// 组织页各组件共享的类型定义（.vue 经 shim 只能默认导入，类型需放在 .ts 中）
import type { OrgView } from '../../api';

export type OrganizationView = OrgView;

export type OrganizationMember = OrgView['members'][number];

export type PluginCatalogItem = {
  id: string;
  domain: string;
  name: string;
  description: string;
  category: 'foundation' | 'business';
  version: string;
  views: string[];
};

export type CreateForm = {
  name: string;
  description: string;
  basePluginDomain: string;
};
