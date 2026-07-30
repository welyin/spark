/**
 * 空间状态与内核接入：模块级响应式单例 spaces（唯一一份，经 contactsOf 访问）、
 * overview 水合、fire-and-forget 持久化（deep watch 兜底）、P2P 事件订阅。
 * 依赖方向：只依赖 types 与 seed，不 import 任何上层业务模块。
 */
import { reactive, watch } from 'vue';
import { isTauri, listenP2pEvents } from '../../api';
import type { FriendDto, FriendRequestDto, OrgInviteRecordDto, P2pEventDto, SpaceContactsDto } from '../../api';
import type { ContactProfile, FriendRequest, MockFriend, SpaceContacts } from './types';
import { emptyProfile } from './types';
import { seedOrg, seedPersonal } from './seed';
import { mockMode } from '../mode';

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

/** 内核 organization API（组织邀请记录水合用；守卫口径同 contactsApi） */
export function organizationApi() {
  if (!isTauri()) {
    return undefined;
  }
  return (window as unknown as { electronAPI?: { organization?: import('../../api').ElectronAPI['organization'] } })
    .electronAPI?.organization;
}

/** 正在水合的空间：水合赋值期间跳过兜底 watch 回写，避免写风暴 */
const hydrating = new Set<string>();

/** DTO → 缓存模型（字段同形，显式拷贝避免引用内核线形对象） */
function toFriend(dto: FriendDto): MockFriend {
  return { ...emptyProfile(), ...dto, phones: [...dto.phones], tagIds: [...dto.tagIds], photos: [...dto.photos] };
}

function toRequest(dto: FriendRequestDto): FriendRequest {
  // updatedAt 内核契约必填（以 DTO 为准）；createdAt 可选，缺省兜底当前时间
  // （混入按时间排序的列表尾部）
  return { createdAt: Date.now(), ...dto };
}

/** 内核组织邀请记录 → 「新的成员」我发出的邀请条目（组织空间 outgoing）。
 *  状态直映射：pending→pending / accepted→accepted / declined→declined（面板文案「已拒绝」）。 */
function toOrgOutgoingInvite(record: OrgInviteRecordDto): FriendRequest {
  return {
    id: record.id,
    rootId: record.peerRootId,
    nickname: record.peerNickname,
    message: `邀请加入「${record.orgName}」`,
    source: 'org-invite',
    status: record.status,
    createdAt: record.createdAt,
    updatedAt: record.updatedAt
  };
}

/** 组织空间我发出的邀请按 id upsert（存在更新 status/updatedAt 等，不存在插入）。 */
function upsertOrgOutgoingInvite(space: SpaceContacts, record: OrgInviteRecordDto): FriendRequest {
  const request = toOrgOutgoingInvite(record);
  const existing = space.outgoing.find((item) => item.id === request.id);
  if (existing) {
    Object.assign(existing, request);
    return existing;
  }
  space.outgoing.push(request);
  return request;
}

/**
 * 发送组织邀请成功后即合入「新的成员 → 我发出的邀请」：空间可能早已水合，
 * 不等下次水合/对方回执事件，面板立即可见刚发出的 pending 记录。
 */
export function applyOrgOutgoingInvite(record: OrgInviteRecordDto): void {
  if (record.direction !== 'outgoing') {
    return;
  }
  upsertOrgOutgoingInvite(contactsOf(`org:${record.orgId}`), record);
}

/**
 * 组织空间水合组织邀请（内核 overview 对组织空间恒空，邀请走独立 IPC）：
 * direction=outgoing 的记录合入「新的成员 → 我发出的邀请」。须在 overview
 * 覆盖赋值完成之后执行，避免被 outgoing 整体替换清掉。
 */
async function hydrateOrgInvites(spaceKey: string, space: SpaceContacts): Promise<void> {
  if (!spaceKey.startsWith('org:')) {
    return;
  }
  const api = organizationApi();
  if (!api) {
    return;
  }
  try {
    const records = await api.inviteRecords(spaceKey.slice('org:'.length));
    hydrating.add(spaceKey);
    try {
      for (const record of records) {
        if (record.direction === 'outgoing') {
          upsertOrgOutgoingInvite(space, record);
        }
      }
    } finally {
      hydrating.delete(spaceKey);
    }
  } catch {
    // 邀请记录拉取失败不打扰：面板退化为仅 overview 数据
  }
}

/**
 * 首次建空间时异步水合 overview：friends/requests/outgoing/tags/groups/groupTree
 * 以服务端为准；requests/outgoing 按 id 合并——服务端同 id 覆盖本地，同时保留
 * 服务端还没有的本地记录（水合窗口内 sendInvite/事件已先落的记录不被抹掉）；
 * memberExtras 用服务端值覆盖同 key、保留本地已惰性新建但服务端没有的 key
 * （水合完成前组件可能已写入本地附加资料）。
 */
function mergeRequestsById(server: FriendRequest[], local: FriendRequest[]): FriendRequest[] {
  const ids = new Set(server.map((request) => request.id));
  return [...server, ...local.filter((request) => !ids.has(request.id))];
}

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
        // 内核不持久化已读状态：重启后待处理的收到申请按未读恢复（仍待我处理，
        // 需要角标/红点提示；查看详情后清除，与在线到达的申请同口径）；
        // 与本地按 id 合并（水合窗口内事件已先落的记录不被抹掉）
        space.requests = mergeRequestsById(
          dto.requests.map((item) => {
            const request = toRequest(item);
            if (request.status === 'pending') {
              request.unread = true;
            }
            return request;
          }),
          space.requests
        );
        space.outgoing = mergeRequestsById(dto.outgoing.map(toRequest), space.outgoing);
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
    .catch(() => {})
    // 组织空间邀请合入排在 overview 覆盖之后（组织空间 overview 恒空，
    // 个人空间此调用为 no-op），两套水合互不覆盖
    .then(() => hydrateOrgInvites(spaceKey, space));
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
 * 通讯录域 P2P 事件处理（个人空间申请事件 + 组织空间邀请状态事件）。
 * 导出供单测直接驱动。
 * - FriendRequestReceived：按 id upsert——同 id 重复到达是对端重试的内容更新，
 *   替换字段并置未读；不存在则新增。
 * - FriendRequestSent：我发出申请的投递终态/对方新询问，按 id upsert outbox
 *   （回填 nickname/status/thread/updatedAt）；事件 updatedAt 早于本地视为过期
 *   快照丢弃（防本地回复被对方询问的旧快照事件回退）；status 'failed'/'replied'
 *   置未读（失败提醒可重试 / 对方新询问=未读新变化）。
 * - FriendRequestAccepted：outbox 置 accepted + 未读，朋友按 rootId 去重落本地。
 * - FriendProfileUpdated：对端资料同步（昵称/头像），按 rootId 就地更新朋友条目；
 *   仅改 nickname/avatar（持久化兜底 watch 只回写本地资料字段，不会把头像写回内核）。
 * - OrgInviteUpdated（组织空间）：管理员收到对方回执，按 record.orgId 解析空间
 *   （`org:{orgId}`）upsert 我发出的邀请，置未读。
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
      // 过期事件守卫：本地已有更新的收敛（如我回复询问置 pending）时，丢弃
      // 携带旧快照的事件（对方询问 replied），避免状态回退/回答行丢失
      if (request.updatedAt < existing.updatedAt) {
        return;
      }
      Object.assign(existing, request);
    } else {
      space.outgoing.push(request);
    }
    const record = existing ?? request;
    if (record.status === 'failed' || record.status === 'replied') {
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
    return;
  }
  if (event.kind === 'FriendProfileUpdated') {
    const { rootId, nickname, avatar } = event.data;
    for (const space of Object.values(spaces)) {
      const friend = space.friends.find((item) => item.rootId === rootId);
      if (!friend) {
        continue;
      }
      friend.nickname = nickname;
      if (avatar !== undefined) {
        friend.avatar = avatar;
      }
    }
    return;
  }
  // 管理员收到对方回执：按 record.orgId 解析组织空间，upsert 我发出的邀请
  // （存在更新 status/updatedAt，不存在插入），对标 FriendRequestAccepted 写法；
  // 状态变化置未读（入口角标提示）。OrgInviteReceived 为被邀请人侧入站通知，
  // 入口是系统会话消息卡片，面板不处理。
  if (event.kind === 'OrgInviteUpdated') {
    const record = event.data;
    if (record.direction !== 'outgoing') {
      return;
    }
    const space = contactsOf(`org:${record.orgId}`);
    upsertOrgOutgoingInvite(space, record).unread = true;
  }
}

/**
 * 演示数据开关：默认关闭（真实内核数据）。
 * 开启方式：`npm run tauri:mock`（VITE_MOCK=1），或 localStorage
 * 'spark:demo-contacts' 置 '1'；置 '0' 则强制关闭（优先级高于环境变量）。
 */
export function demoContacts(): boolean {
  try {
    const override = localStorage.getItem('spark:demo-contacts');
    if (override === '1') {
      return true;
    }
    if (override === '0') {
      return false;
    }
  } catch {
    // localStorage 不可用时跟随环境变量
  }
  return mockMode();
}

/** 取（并按需建空 + 水合）某空间的通讯录数据；同一空间 key 恒得同一响应式对象 */
export function contactsOf(spaceKey: string): SpaceContacts {
  if (!spaces[spaceKey]) {
    if (!isTauri() || demoContacts()) {
      // 非 Tauri（单测/纯前端开发）或 mock 模式：本地种子数据，完全不触网
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
