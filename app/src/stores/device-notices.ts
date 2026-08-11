/**
 * 新设备加入通知（M1，m1-m2-implementation-plan §3.4）。
 *
 * 同身份新设备配对成功后，既有设备经自设备 DM 管道收到 DeviceNoticeReceived
 * 事件（内核是无状态管道，每次收到合法通知都发事件）——去重在本模块按
 * deviceId 做：同设备重复事件仅更新 ts，不重复弹提示。
 *
 * 待看通知持久化到 localStorage（键 spark:device-notices:<rootId>，按身份
 * 隔离），重启后红点仍在；进入设备管理页（DevicesModule onMounted 调
 * markDeviceNoticesSeen）清空。读写一律 try/catch：隐私模式访问即抛，
 * 降级为当次会话内存态，不能拖垮应用（coding-standards §4.4）。
 */
import { ref } from 'vue';

export interface DeviceNotice {
  deviceId: string;
  deviceName: string;
  ts: number;
}

const STORAGE_KEY_PREFIX = 'spark:device-notices:';

/** 当前身份的待看新设备通知（设备管理入口红点数据源，MinePage/SettingsPage 消费） */
export const pendingDeviceNotices = ref<DeviceNotice[]>([]);

/** 已水合的身份（同身份重复水合幂等；切换账号整窗重载，无需运行时切换） */
let hydratedRootId: string | null = null;
/** 本机 peerId（DevicesModule load() 后回写）：本机的加入通知直接忽略 */
let currentDevicePeerId: string | null = null;

function load(rootId: string): DeviceNotice[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY_PREFIX + rootId);
    if (!raw) {
      return [];
    }
    const parsed = JSON.parse(raw) as Array<Partial<DeviceNotice>>;
    if (!Array.isArray(parsed)) {
      return [];
    }
    // 数据损坏容忍：逐条校验字段，坏条目丢弃不拖垮整单
    return parsed.filter(
      (item): item is DeviceNotice =>
        typeof item?.deviceId === 'string' && item.deviceId.length > 0 &&
        typeof item?.deviceName === 'string' && typeof item?.ts === 'number'
    );
  } catch {
    return [];
  }
}

function persist(rootId: string): void {
  try {
    localStorage.setItem(STORAGE_KEY_PREFIX + rootId, JSON.stringify(pendingDeviceNotices.value));
  } catch {
    // 持久化失败不阻断当次提示（重启后红点丢失，可接受）
  }
}

/**
 * 水合指定身份的待看通知（App 挂载拿到 rootId 后调用；同身份重复调用幂等）。
 * 合并语义：rootId 未就绪的窗口内到达的事件已先入内存（未持久化），水合不能整体
 * 替换——持久化列表 ∪ 内存项按 deviceId 去重（同设备取 ts 较新者），合并结果落盘，
 * 保证红点跨重启可恢复。
 */
export function hydrateDeviceNotices(rootId: string): void {
  if (!rootId || hydratedRootId === rootId) {
    return;
  }
  hydratedRootId = rootId;
  const persisted = load(rootId);
  const inMemory = pendingDeviceNotices.value;
  if (!inMemory.length) {
    pendingDeviceNotices.value = persisted;
    return;
  }
  const mergedByDeviceId = new Map<string, DeviceNotice>();
  for (const notice of [...persisted, ...inMemory]) {
    const existing = mergedByDeviceId.get(notice.deviceId);
    if (!existing || notice.ts > existing.ts) {
      mergedByDeviceId.set(notice.deviceId, notice);
    }
  }
  pendingDeviceNotices.value = [...mergedByDeviceId.values()];
  persist(rootId);
}

/** 回写本机 peerId（DevicesModule load() 后取 isSelf 记录调用） */
export function setCurrentDevicePeerId(peerId: string | null): void {
  currentDevicePeerId = peerId;
}

/**
 * DeviceNoticeReceived 事件入口。
 * 返回 true = 新设备（调用方弹提示）；false = 重复通知（仅更新 ts）或本机/空记录（忽略）。
 */
export function handleDeviceNotice(rootId: string, data: DeviceNotice): boolean {
  if (!data.deviceId || data.deviceId === currentDevicePeerId) {
    return false;
  }
  if (rootId) {
    hydrateDeviceNotices(rootId);
  }
  const existing = pendingDeviceNotices.value.find((notice) => notice.deviceId === data.deviceId);
  pendingDeviceNotices.value = existing
    ? pendingDeviceNotices.value.map((notice) =>
        notice.deviceId === data.deviceId ? { ...notice, ts: data.ts } : notice
      )
    : [...pendingDeviceNotices.value, { deviceId: data.deviceId, deviceName: data.deviceName, ts: data.ts }];
  if (rootId) {
    persist(rootId);
  }
  return !existing;
}

/** 清空待看通知并持久化（设备管理页挂载时调用，红点即清） */
export function markDeviceNoticesSeen(rootId: string): void {
  if (!pendingDeviceNotices.value.length) {
    return;
  }
  pendingDeviceNotices.value = [];
  if (rootId) {
    persist(rootId);
  }
}
