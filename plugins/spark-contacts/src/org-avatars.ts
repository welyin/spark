/**
 * 组织 logo 展示缓存——插件 v1 桩：壳层 org-avatars 为 localStorage 持久化的
 * 展示缓存；插件 iframe 为 opaque origin 无 localStorage，且组织 logo 在
 * 通讯录面非关键路径，v1 恒空（OrgView.avatar 由 org-membership 直通消费）。
 */
import { ref } from 'vue';

/** 组织 logo 缓存（v1 恒空） */
export const orgAvatars = ref<Record<string, string>>({});

/** 写入缓存（v1 no-op） */
export function setOrgAvatar(_orgId: string, _avatar: string): void {}
