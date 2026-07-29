/**
 * 空间状态与内核接入：模块级响应式单例 spaces（唯一一份，经 contactsOf 访问）、
 * overview 水合、fire-and-forget 持久化（deep watch 兜底）、P2P 事件订阅。
 * 依赖方向：只依赖 types 与 seed，不 import 任何上层业务模块。
 */
import { reactive, watch } from 'vue';
import { isTauri, listenP2pEvents } from '../../api';
import type { FriendDto, FriendRequestDto, P2pEventDto, SpaceContactsDto } from '../../api';
import type { ContactProfile, FriendRequest, MockFriend, SpaceContacts } from './types';
import { emptyProfile } from './types';
import { seedOrg, seedPersonal } from './seed';

/** 全部空间的通讯录缓存：同一空间 key 恒得同一响应式对象（模块级唯一单例） */
export const spaces = reactive<Record<string, SpaceContacts>>({});

/** 内核 contacts API（非 Tauri 环境为 undefined，全调用点守卫 + catch 静默） */
export function contactsApi() {
  if (!isTauri()) {
    return undefined;
  }
  return (window as unknown as { electronAPI?: { contacts?: import('../../api').ElectronAPI['contacts'] } })
    .electronAPI?.contacts;
}

/** 正在水合的空间：水合赋值期间跳过兜底 watch 回写，避免写风暴 */
const hydrating = new Set<string>();

/** DTO → 缓存模型（字段同形，显式拷贝避免引用内核线形对象） */
function toFriend(dto: FriendDto): MockFriend {
  return { ...emptyProfile(), ...dto, phones: [...dto.phones], tagIds: [...dto.tagIds], photos: [...dto.photos] };
}

function toRequest(dto: FriendRequestDto): FriendRequest {
  // updatedAt 内核契约必填，createdAt 可选；缺省时兜底为当前时间（混入按时间
  // 排序的列表尾部），有值则以 DTO 为准（展开在兜底之后，覆盖兜底值）
  return { createdAt: Date.now(), updatedAt: Date.now(), ...dto };
}

/**
 * 首次建空间时异步水合 overview：friends/requests/outgoing/tags/groups/groupTree
 * 直接以服务端为准替换；memberExtras 用服务端值覆盖同 key、保留本地已惰性新建
 * 但服务端没有的 key（水合完成前组件可能已写入本地附加资料）。
 */
function hydrate(spaceKey: string, space: SpaceContacts): void {
  const api = contactsApi();
  if (!api) {
    return;
  }
  api
    .overview(spaceKey)
    .then((dto: SpaceContactsDto) => {
      hydrating.add(spaceKey);
      try {
        space.friends = dto.friends.map(toFriend);
        space.requests = dto.requests.map(toRequest);
        space.outgoing = dto.outgoing.map(toRequest);
        space.tags = dto.tags.map((tag) => ({ ...tag }));
        space.groups = dto.groups.map((group) => ({ ...group }));
        space.groupTree = dto.groupTree;
        for (const [rootId, profile] of Object.entries(dto.memberExtras)) {
          space.memberExtras[rootId] = { ...emptyProfile(), ...profile };
        }
      } finally {
        hydrating.delete(spaceKey);
      }
    })
    .catch(() => {});
}

/**
 * 直写路径兜底：组件存在绕过函数直改响应式对象的情况（TagManager 直接
 * push/filter profileOf(...).tagIds），函数级持久化覆盖不到。对每个空间挂一次
 * deep watch，debounce 500ms 后把该空间全部 friend 与 memberExtras 的资料整体
 * updateProfile 持久化（量级每空间几十条内）。
 * 水合赋值期间（hydrating 标志）跳过，避免水合触发的回写风暴。
 */
const watchedSpaces = new Set<string>();

function ensurePersistWatch(spaceKey: string): void {
  if (watchedSpaces.has(spaceKey)) {
    return;
  }
  watchedSpaces.add(spaceKey);
  let timer: ReturnType<typeof setTimeout> | undefined;
  watch(
    () => spaces[spaceKey],
    () => {
      if (!isTauri() || hydrating.has(spaceKey)) {
        return;
      }
      if (timer) {
        clearTimeout(timer);
      }
      timer = setTimeout(() => {
        timer = undefined;
        const api = contactsApi();
        const space = spaces[spaceKey];
        if (!api || !space || hydrating.has(spaceKey)) {
          return;
        }
        const persist = (rootId: string, profile: ContactProfile) => {
          const { remark, phones, tagIds, groupId, memo, photos, permission, blocked } = profile;
          api.updateProfile(spaceKey, rootId, {
            remark,
            phones: [...phones],
            tagIds: [...tagIds],
            groupId,
            memo,
            photos: [...photos],
            permission,
            blocked
          }).catch(() => {});
        };
        space.friends.forEach((friend) => persist(friend.rootId, friend));
        Object.entries(space.memberExtras).forEach(([rootId, profile]) => persist(rootId, profile));
      }, 500);
    },
    // flush: 'sync' 让回调在水合赋值的同一同步段内触发，hydrating 标志才能
    // 真正拦住水合引发的回写（默认 pre flush 异步执行时标志已复位）
    { deep: true, flush: 'sync' }
  );
}

// ------------------------------------------------------------------
// P2P 事件订阅（模块级懒初始化：首次 contactsOf 且 Tauri 环境）
// ------------------------------------------------------------------

let eventsSubscribed = false;

function ensureEventSubscription(): void {
  if (eventsSubscribed || !isTauri()) {
    return;
  }
  eventsSubscribed = true;
  listenP2pEvents(handleContactsP2pEvent).catch(() => {
    eventsSubscribed = false;
  });
}

/**
 * 通讯录域 P2P 事件处理（个人空间）。导出供单测直接驱动。
 * - FriendRequestReceived：按 id upsert——同 id 重复到达是对端重试的内容更新，
 *   替换字段并置未读；不存在则新增。
 * - FriendRequestSent：我发出申请的投递终态，按 id upsert outbox（回填 nickname/
 *   status/updatedAt）；status 'failed' 置未读提醒可重试。
 * - FriendRequestAccepted：outbox 置 accepted + 未读，朋友按 rootId 去重落本地。
 */
export function handleContactsP2pEvent(event: P2pEventDto): void {
  if (event.kind === 'FriendRequestReceived') {
    const space = contactsOf('personal');
    const request = toRequest(event.data.request);
    const existing = space.requests.find((item) => item.id === request.id);
    if (existing) {
      Object.assign(existing, request);
      existing.unread = true;
    } else {
      request.unread = true;
      space.requests.push(request);
    }
    return;
  }
  if (event.kind === 'FriendRequestSent') {
    const space = contactsOf('personal');
    const request = toRequest(event.data.request);
    const existing = space.outgoing.find((item) => item.id === request.id);
    if (existing) {
      Object.assign(existing, request);
    } else {
      space.outgoing.push(request);
    }
    const record = existing ?? request;
    if (record.status === 'failed') {
      record.unread = true;
    }
    return;
  }
  if (event.kind === 'FriendRequestAccepted') {
    const space = contactsOf('personal');
    const outgoing = space.outgoing.find((item) => item.id === event.data.request.id);
    if (outgoing) {
      outgoing.status = 'accepted';
      outgoing.updatedAt = event.data.request.updatedAt ?? Date.now();
      outgoing.unread = true;
    }
    const friend = toFriend(event.data.friend);
    if (!space.friends.some((item) => item.rootId === friend.rootId)) {
      space.friends.push(friend);
    }
  }
}

/**
 * 开发环境演示数据开关（localStorage 'spark:demo-contacts'）：
 * dev 构建默认开——即使 Tauri 环境也用种子假数据、跳过内核水合，
 * 便于 UI 调试；置 '0' 后刷新即可回到内核真实数据。生产构建恒 false。
 */
export function demoContacts(): boolean {
  if (!import.meta.env.DEV) {
    return false;
  }
  try {
    return localStorage.getItem('spark:demo-contacts') !== '0';
  } catch {
    return true;
  }
}

/** 取（并按需建空 + 水合）某空间的通讯录数据；同一空间 key 恒得同一响应式对象 */
export function contactsOf(spaceKey: string): SpaceContacts {
  if (!spaces[spaceKey]) {
    if (!isTauri() || demoContacts()) {
      // 非 Tauri（单测/纯前端开发）或 dev 演示模式：本地种子数据，完全不触网
      spaces[spaceKey] = spaceKey === 'personal' ? seedPersonal() : seedOrg();
      return spaces[spaceKey];
    }
    spaces[spaceKey] = {
      friends: [],
      requests: [],
      outgoing: [],
      tags: [],
      groups: [],
      groupTree: [],
      memberExtras: {}
    };
    ensureEventSubscription();
    ensurePersistWatch(spaceKey);
    hydrate(spaceKey, spaces[spaceKey]);
  }
  return spaces[spaceKey];
}
