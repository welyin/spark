/**
 * 通讯录查询（`contact:read` 只读面，social-feed §9.4）：好友关系判定
 * 「联系人已删除」（评审 H1 修复，对齐壳层 friendOf 语义）。
 *
 * 数据面：sdk.contacts.listFriends() 水合按空间缓存的好友 rootId 集合，
 * sdk.contacts.onChanged 订阅失效后重读收敛。space 由桥绑定注入（适配层
 * 忽略 spaceKey 实参），spaceKey 仅作缓存分桶键。
 *
 * 状态三态：加载中/未知（loaded=false，未授权或查询失败亦保持此态）与
 * 「已删除」严格区分——未拿到权威名单前不得误判已删除（不禁发、不显
 * 「此人已删除」），消费方一律走 `isContactDeleted`。
 *
 * 未绑定 SDK（vitest / 纯前端预览）恒为未知态（纯内存降级，不禁止发送）。
 */
import { reactive } from 'vue';
import { contactsApi } from './sdk-host';

export interface FriendRef {
  rootId: string;
}

interface ContactsCache {
  /** 好友 rootId 集合（响应式）；loaded=false 时内容不可信 */
  friends: Set<string>;
  /** 是否已完成首次水合（false = 加载中/未知，不得误判已删除） */
  loaded: boolean;
  /** 是否有水合请求在途（失败后下次访问重试） */
  inflight: boolean;
  /** 是否已订阅 onChanged（每空间一次） */
  subscribed: boolean;
}

const caches = reactive<Record<string, ContactsCache>>({});

function ensureCache(spaceKey: string): ContactsCache {
  let cache = caches[spaceKey];
  if (!cache) {
    cache = caches[spaceKey] = {
      friends: reactive(new Set<string>()),
      loaded: false,
      inflight: false,
      subscribed: false
    };
  }
  if (!cache.loaded && !cache.inflight) {
    void hydrate(spaceKey, cache);
  }
  return cache;
}

async function hydrate(spaceKey: string, cache: ContactsCache): Promise<void> {
  const api = contactsApi();
  if (!api) return; // 未绑定 SDK：保持未知态
  cache.inflight = true;
  try {
    const friends = await api.listFriends();
    cache.friends.clear();
    for (const f of friends) {
      cache.friends.add(f.rootId);
    }
    cache.loaded = true;
  } catch {
    // 权限未授予 / 查询失败：保持未知态，不误判已删除
  } finally {
    cache.inflight = false;
  }
  if (!cache.subscribed) {
    cache.subscribed = true;
    void api.onChanged(() => {
      cache.loaded = false;
      void hydrate(spaceKey, cache);
    });
  }
}

/**
 * 查询好友关系：命中返回 FriendRef；未命中返回 undefined（含未知态——
 * 调用方不得以 undefined 直接判定「已删除」，判定用 isContactDeleted）。
 * 首次访问触发异步水合，完成后经响应式缓存自动刷新视图。
 */
export function friendOf(spaceKey: string, rootId: string): FriendRef | undefined {
  const cache = ensureCache(spaceKey);
  if (!cache.loaded || !cache.friends.has(rootId)) return undefined;
  return { rootId };
}

/**
 * 「联系人已删除」权威判定：仅当好友名单已水合且 rootId 不在其中时为 true。
 * 加载中 / 未授权 / 查询失败（未知态）一律 false——不禁止发送、不显示
 * 「此人已删除」。
 */
export function isContactDeleted(spaceKey: string, rootId: string): boolean {
  const cache = ensureCache(spaceKey);
  if (!cache.loaded) return false;
  return !cache.friends.has(rootId);
}

/** 登录态切换 / 测试隔离：清空全部空间通讯录缓存 */
export function resetContactsCache(): void {
  for (const key of Object.keys(caches)) {
    delete caches[key];
  }
}
