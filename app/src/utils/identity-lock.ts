/**
 * 账号登出/切换账号的统一收敛点（device-trust-and-biometric 落地路线 §6「登出 4 套实现收敛为 1 套」）。
 *
 * 语义：锁身份（rootIdentity.lock）+ 整窗重载回 RootGate 登录门。
 * 主界面（App.vue 顶栏 / SettingsPage）退出与切换账号共用此函数；
 * RootGate 内不整窗重载（其已处于 gate 内，直接在门内回落登录态），不调用本工具。
 */
import { ElMessage } from 'element-plus';

export async function lockAndReload(successText: string): Promise<void> {
  try {
    await window.electronAPI.rootIdentity.lock();
    ElMessage.success(successText);
    window.location.reload();
  } catch (error) {
    ElMessage.error(`操作失败：${error}`);
  }
}
