/**
 * 跨域拖放（problem X1/X2/X4）——壳层可拖对象 → 目标域 的判定与确认管线。
 *
 * 现状说明（诚实口径）：
 * - 壳层当前唯一可拖对象源 = 桌面图标（DesktopIconGrid pointer 拖动）。插件 iframe 内的
 *   对象无法跨 iframe 边界参与壳层拖拽（X1 对象源缺口，需插件 SDK 拖出协议支持）。
 * - 「落点即域」：目标域只认空间引用（个人 / orgId），同空间松手不弹（X1），
 *   跨域松手一律弹 CrossDomainDropConfirm（X1/X2），可勾「本次会话同一 源→目标 不再提示」——
 *   豁免仅存活于本模块内存（关会话即失效），无 localStorage 持久化 = 无永久豁免（X2 口径）。
 * - 确认后的实际投递语义（把对象发到目标域）需目标插件/内核承接，本 store 只派发
 *   `spark:cross-domain-drop` 事件作为统一收敛点；无消费者时为安全的空操作。
 *
 * X4 拖放态反馈：拖拽期间 dragFeedback 始终携带 目标域名 + 可见范围 + 是否可承接，
 * 由 DropFeedbackHint 跟随光标渲染；不可承接处 allowed=false（配合全局 no-drop 光标）。
 */
import { computed, ref } from 'vue';
import { findOrg } from './org-membership';

/** 空间引用（与 current-space.DeepLinkSpace 同形，独立声明避免 store 间类型耦合） */
export type SpaceRef = { type: 'personal' } | { type: 'org'; orgId: string };

/** 壳层可拖对象描述（当前仅桌面应用图标；插件对象待 SDK 拖出协议） */
export interface DropObject {
  kind: 'app-icon';
  id: string;
  label: string;
  source: SpaceRef;
}

/** 确认后的投递事件名（统一收敛点；消费方待插件/内核承接） */
export const CROSS_DOMAIN_DROP_EVENT = 'spark:cross-domain-drop';

/** 拖拽会话（进行中）：对象 + 光标位置 + 当前悬停落点 */
export interface DragSession {
  object: DropObject;
  x: number;
  y: number;
  /** 悬停落点域；null = 不在任何可承接落点上 */
  hover: SpaceRef | null;
  /** 悬停处是否可承接（false → 禁止光标） */
  hoverAllowed: boolean;
}

/** 待确认的跨域投递（CrossDomainDropConfirm 数据源） */
export interface PendingCrossDomainDrop {
  object: DropObject;
  target: SpaceRef;
}

const session = ref<DragSession | null>(null);
const pending = ref<PendingCrossDomainDrop | null>(null);

/** 本次会话内的「同一 源→目标 不再提示」豁免集（内存态，关会话即失效） */
const sessionExemptions = new Set<string>();

/** 空间引用的稳定键（豁免集 / 比较用） */
export function spaceKeyOfRef(space: SpaceRef): string {
  return space.type === 'personal' ? 'personal' : `org:${space.orgId}`;
}

export function sameSpace(a: SpaceRef, b: SpaceRef): boolean {
  return spaceKeyOfRef(a) === spaceKeyOfRef(b);
}

/** 目标域展示名（个人空间 / 组织名） */
export function spaceLabelOf(space: SpaceRef): string {
  if (space.type === 'personal') {
    return '个人空间';
  }
  return findOrg(space.orgId)?.name ?? '组织空间';
}

/** X4 可见范围口径：个人空间仅自己可见；组织空间将同步给成员 */
export function scopeLabelOf(space: SpaceRef): string {
  return space.type === 'personal' ? '仅自己可见' : '将同步给该组织成员';
}

/** 当前拖拽会话（供 DropFeedbackHint / 落点高亮读取） */
export const dragSession = computed(() => session.value);

/** 待确认投递（CrossDomainDropConfirm v-model 数据源） */
export const pendingCrossDomainDrop = computed(() => pending.value);

/** 某空间项是否正处于拖拽悬停（rail 落点高亮） */
export function isDropHover(space: SpaceRef): boolean {
  const s = session.value;
  return !!s && !!s.hover && s.hoverAllowed && sameSpace(s.hover, space);
}

/** 开始一次对象拖拽（桌面图标 pointer 拖动越过阈值时调用） */
export function beginObjectDrag(object: DropObject, x: number, y: number): void {
  session.value = { object, x, y, hover: null, hoverAllowed: false };
}

/** 拖拽移动：更新光标位置与悬停落点（落点由调用方 hit-test 得出） */
export function updateObjectDrag(x: number, y: number, hover: SpaceRef | null, allowed: boolean): void {
  if (!session.value) {
    return;
  }
  session.value = { ...session.value, x, y, hover, hoverAllowed: hover ? allowed : false };
}

/** 取消拖拽（Esc / 未落在任何落点松手 / 确认被取消后收尾） */
export function endObjectDrag(): void {
  session.value = null;
}

/**
 * 松手请求投递。返回：
 * - 'none'：无拖拽会话或不在落点上（不产生任何动作）
 * - 'committed'：同空间（X1 不弹）或已豁免的 源→目标（X2 本次会话不再提示），直接投递
 * - 'confirming'：跨域首遇，已打开确认框（结果经 confirmCrossDomainDrop / cancelCrossDomainDrop）
 */
export function requestDrop(target: SpaceRef): 'none' | 'committed' | 'confirming' {
  const s = session.value;
  session.value = null;
  if (!s || !s.hover || !s.hoverAllowed) {
    return 'none';
  }
  if (sameSpace(s.object.source, target)) {
    commitDrop(s.object, target);
    return 'committed';
  }
  const exemptionKey = `${spaceKeyOfRef(s.object.source)}→${spaceKeyOfRef(target)}`;
  if (sessionExemptions.has(exemptionKey)) {
    commitDrop(s.object, target);
    return 'committed';
  }
  pending.value = { object: s.object, target };
  return 'confirming';
}

/** 确认跨域投递；remember=true 记入本次会话豁免（同源→同目标不再提示） */
export function confirmCrossDomainDrop(remember: boolean): void {
  const p = pending.value;
  pending.value = null;
  if (!p) {
    return;
  }
  if (remember) {
    sessionExemptions.add(`${spaceKeyOfRef(p.object.source)}→${spaceKeyOfRef(p.target)}`);
  }
  commitDrop(p.object, p.target);
}

/** 取消跨域投递（不写豁免） */
export function cancelCrossDomainDrop(): void {
  pending.value = null;
}

/** 投递统一收敛点：实际跨域写入待目标插件/内核承接（无消费者为空操作） */
function commitDrop(object: DropObject, target: SpaceRef): void {
  window.dispatchEvent(
    new CustomEvent(CROSS_DOMAIN_DROP_EVENT, {
      detail: { object, from: object.source, to: target }
    })
  );
}

/** 测试辅助：清空会话豁免（豁免本就随会话失效，测试间需显式重置） */
export function resetCrossDomainDropForTest(): void {
  session.value = null;
  pending.value = null;
  sessionExemptions.clear();
}
