/**
 * 当前登录用户资料（模块级单例响应式对象）。
 *
 * 所有「个人头像」共用同一数据源：rail 头像、空间切换器、消息气泡里自己的头像等。
 * App 挂载时刷新一次，个人资料更新（profile-updated）后再刷新。
 */
import { reactive } from 'vue';

export const currentUser = reactive<{ rootId: string | null; nickname: string; avatar: string }>({
  rootId: null,
  nickname: '',
  avatar: ''
});

/** 从内核读取最新资料写入单例；读取失败保留现状（对齐原 App.loadCurrentUser 语义） */
export async function refreshCurrentUser(): Promise<void> {
  try {
    const status = await window.electronAPI.rootIdentity.status();
    currentUser.rootId = status.rootId;
    currentUser.nickname = status.nickname ?? '';
    currentUser.avatar = status.avatar ?? '';
  } catch {
    // 读取失败时保留默认自动头像
  }
}
