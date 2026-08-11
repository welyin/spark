// M1 新设备加入通知 store（stores/device-notices）纯逻辑单测。
//
// 覆盖方案文档 §6.16（store 层）：
// - handleDeviceNotice 返回 true=新设备 / false=重复或本机或空记录；
// - 同 deviceId 重复通知：不重复弹（返回 false），仅更新 ts；
// - 本机 peerId（setCurrentDevicePeerId 设置后）到达的加入通知直接忽略；
// - hydrateDeviceNotices 从 localStorage 水合、同身份幂等；
// - markDeviceNoticesSeen 清空并持久化（红点即清）；
// - pendingDeviceNotices 是待看红点数据源。
//
// 模块级单例 + localStorage：每个用例用唯一 rootId 驱动，避免相互污染；
// 用例间重置 pendingDeviceNotices.value 与 localStorage。
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  handleDeviceNotice,
  hydrateDeviceNotices,
  markDeviceNoticesSeen,
  pendingDeviceNotices,
  setCurrentDevicePeerId
} from '../../stores/device-notices';

let rootSeq = 0;
function freshRoot(): string {
  rootSeq += 1;
  return `root-notice-${rootSeq}`;
}

beforeEach(() => {
  localStorage.clear();
  // 重置模块级单例：pending 清空、本机 peerId 置空。
  markDeviceNoticesSeen(freshRoot());
  setCurrentDevicePeerId(null);
});

afterEach(() => {
  pendingDeviceNotices.value = [];
  setCurrentDevicePeerId(null);
});

describe('handleDeviceNotice（M1 store 层）', () => {
  it('新设备返回 true 并进待看列表（红点数据源）', () => {
    const root = freshRoot();
    const isNew = handleDeviceNotice(root, {
      deviceId: 'peer-new-1',
      deviceName: '新手机',
      ts: 1000
    });
    expect(isNew).toBe(true);
    expect(pendingDeviceNotices.value).toHaveLength(1);
    expect(pendingDeviceNotices.value[0]).toEqual({
      deviceId: 'peer-new-1',
      deviceName: '新手机',
      ts: 1000
    });
  });

  it('同 deviceId 重复通知不重复弹（返回 false），仅更新 ts', () => {
    const root = freshRoot();
    expect(
      handleDeviceNotice(root, { deviceId: 'peer-new-2', deviceName: 'P2', ts: 1000 })
    ).toBe(true);
    // 同一设备再次通知（如内核重发/重连补发）→ 不重复弹，仅刷新 ts。
    expect(
      handleDeviceNotice(root, { deviceId: 'peer-new-2', deviceName: 'P2', ts: 2000 })
    ).toBe(false);
    expect(pendingDeviceNotices.value).toHaveLength(1);
    expect(pendingDeviceNotices.value[0].ts).toBe(2000);
  });

  it('本机 peerId 的加入通知直接忽略（返回 false）', () => {
    const root = freshRoot();
    setCurrentDevicePeerId('peer-self-device');
    expect(
      handleDeviceNotice(root, { deviceId: 'peer-self-device', deviceName: '本机', ts: 1000 })
    ).toBe(false);
    expect(pendingDeviceNotices.value).toHaveLength(0);
  });

  it('空 deviceId 的记录忽略', () => {
    const root = freshRoot();
    expect(handleDeviceNotice(root, { deviceId: '', deviceName: 'x', ts: 1000 })).toBe(false);
    expect(pendingDeviceNotices.value).toHaveLength(0);
  });

  it('多台新设备各自独立计入', () => {
    const root = freshRoot();
    handleDeviceNotice(root, { deviceId: 'p-a', deviceName: 'A', ts: 1 });
    handleDeviceNotice(root, { deviceId: 'p-b', deviceName: 'B', ts: 2 });
    expect(pendingDeviceNotices.value).toHaveLength(2);
  });
});

describe('hydrate / persist / markSeen（M1 红点生命周期）', () => {
  it('重启后可从 localStorage 水合（按身份隔离）', () => {
    const root = freshRoot();
    // 模拟上次会话持久化（重启后内存 pending 为空，hydratedRootId 未设置）。
    localStorage.setItem(
      `spark:device-notices:${root}`,
      JSON.stringify([{ deviceId: 'peer-h', deviceName: 'H', ts: 500 }])
    );
    expect(pendingDeviceNotices.value).toHaveLength(0);
    hydrateDeviceNotices(root);
    expect(pendingDeviceNotices.value).toHaveLength(1);
    expect(pendingDeviceNotices.value[0].deviceId).toBe('peer-h');
  });

  it('不同 rootId 的身份隔离：互不串台', () => {
    const rootA = freshRoot();
    const rootB = freshRoot();
    handleDeviceNotice(rootA, { deviceId: 'peer-a', deviceName: 'A', ts: 1 });
    handleDeviceNotice(rootB, { deviceId: 'peer-b', deviceName: 'B', ts: 2 });
    pendingDeviceNotices.value = [];
    hydrateDeviceNotices(rootA);
    expect(pendingDeviceNotices.value.map((n) => n.deviceId)).toEqual(['peer-a']);
  });

  it('markDeviceNoticesSeen 清空待看（进入设备页红点即清）', () => {
    const root = freshRoot();
    handleDeviceNotice(root, { deviceId: 'peer-clear', deviceName: 'C', ts: 1 });
    expect(pendingDeviceNotices.value).toHaveLength(1);
    markDeviceNoticesSeen(root);
    expect(pendingDeviceNotices.value).toHaveLength(0);
  });

  it('损坏的 localStorage 数据：坏条目被丢弃不拖垮', () => {
    const root = freshRoot();
    localStorage.setItem(
      `spark:device-notices:${root}`,
      JSON.stringify([
        { deviceId: 'good', deviceName: 'G', ts: 1 },
        { deviceId: null, deviceName: 'bad-no-id' },
        'not-an-object',
        { deviceId: 'no-ts' }
      ])
    );
    hydrateDeviceNotices(root);
    expect(pendingDeviceNotices.value).toHaveLength(1);
    expect(pendingDeviceNotices.value[0].deviceId).toBe('good');
  });

  it('水合合并语义（§6-7 竞态）：rootId 未就绪的窗口事件 ∪ 持久化列表去重落盘', () => {
    const root = freshRoot();
    // rootId 未就绪窗口：空 rootId 入内存（不持久化）。
    expect(handleDeviceNotice('', { deviceId: 'in-mem-a', deviceName: 'A', ts: 10 })).toBe(true);
    expect(pendingDeviceNotices.value).toHaveLength(1);
    // localStorage 预置另一台（同身份上次会话遗留）。
    localStorage.setItem(
      `spark:device-notices:${root}`,
      JSON.stringify([{ deviceId: 'persisted-b', deviceName: 'B', ts: 5 }])
    );
    // 水合 → 合并 a∪b，且同 deviceId 取 ts 较新。
    hydrateDeviceNotices(root);
    const ids = pendingDeviceNotices.value.map((n) => n.deviceId);
    expect(ids).toContain('in-mem-a');
    expect(ids).toContain('persisted-b');
    expect(pendingDeviceNotices.value).toHaveLength(2);
    // 合并结果落盘（重启可恢复）。
    const persisted = JSON.parse(
      localStorage.getItem(`spark:device-notices:${root}`)!
    ) as Array<{ deviceId: string }>;
    expect(persisted.map((n) => n.deviceId)).toEqual(expect.arrayContaining(ids));
  });

  it('水合合并：同 deviceId 冲突取 ts 较新者', () => {
    const root = freshRoot();
    // 内存已有较旧，持久化已有较新 → 合并取较新。
    handleDeviceNotice('', { deviceId: 'dup', deviceName: 'D', ts: 10 });
    localStorage.setItem(
      `spark:device-notices:${root}`,
      JSON.stringify([{ deviceId: 'dup', deviceName: 'D', ts: 50 }])
    );
    hydrateDeviceNotices(root);
    expect(pendingDeviceNotices.value).toHaveLength(1);
    expect(pendingDeviceNotices.value[0].ts).toBe(50);
  });
});
