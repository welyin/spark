/**
 * 跨页面「打开系统设置指定模块」请求（Android 顶部导航改造）。
 *
 * 顶部导航网络状态点等入口请求直达「设置→系统设置→网络状态」：写入本模块，
 * App.vue 切到设置 tab，SettingsPage 挂载后消费并定位到指定 section。
 */
import { ref } from 'vue';

/** 系统设置可定位的 section 键（与 SystemSettingsPanel.SectionKey 对齐） */
export type SystemSectionKey = 'netStatus' | 'devices' | 'storage' | 'general' | 'notify' | 'about';

/** 待消费的打开系统设置指定模块请求；SettingsPage 消费后置空 */
export const pendingSystemSection = ref<SystemSectionKey | null>(null);

export function requestOpenSystemSection(section: SystemSectionKey): void {
  pendingSystemSection.value = section;
}

/** 取出并清空当前请求（无请求时返回 null） */
export function consumePendingSystemSection(): SystemSectionKey | null {
  const section = pendingSystemSection.value;
  pendingSystemSection.value = null;
  return section;
}
