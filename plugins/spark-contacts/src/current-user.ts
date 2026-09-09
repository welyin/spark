/**
 * 当前登录用户资料（插件版，模块级单例响应式对象；与壳层 stores/current-user
 * 同形状）：数据源 sdk.runtime.currentRoot（免权限基础调用），插件入口
 * 握手后刷新一次（index.ts），失败保留现状。
 */
import { reactive } from 'vue';
import { currentRoot } from './sdk-host';

export const currentUser = reactive<{ rootId: string | null; nickname: string; avatar: string }>({
  rootId: null,
  nickname: '',
  avatar: ''
});

/** 从内核读取最新资料写入单例；读取失败保留现状（对齐壳层 refreshCurrentUser 语义） */
export async function refreshCurrentUser(): Promise<void> {
  try {
    const status = await currentRoot();
    currentUser.rootId = status.rootId;
    currentUser.nickname = status.nickname ?? '';
    currentUser.avatar = status.avatar ?? '';
  } catch {
    // 读取失败时保留默认自动头像
  }
}
