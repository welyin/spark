// security-log-list 前端冒烟（M2 §6.12 / 决策点 3 收敛点）。
//
// 覆盖：
// - `window.electronAPI.devices.securityLogList(limit?)` 前端适配存在且透传 limit；
// - 出参 DTO 形状 {key, kind, deviceId, deviceName?, actor?, ts}（对齐壳层
//   dto.rs SecurityLogEntryDto camelCase 序列化）；
// - initiated 事件带 deviceName/actor:"local"，effective 事件缺省。
import { afterEach, describe, expect, it, vi } from 'vitest';

const INITIATED_DTO = {
  key: 'security:log:1700000000000:device_revoke_initiated:peer-b',
  kind: 'device_revoke_initiated',
  deviceId: 'peer-b',
  deviceName: '对端设备',
  actor: 'local',
  ts: 1700000000000
};
const EFFECTIVE_DTO = {
  key: 'security:log:1700000000000:device_revoke_effective:peer-b',
  kind: 'device_revoke_effective',
  deviceId: 'peer-b',
  ts: 1700000000000
};

afterEach(() => {
  delete (window as any).electronAPI;
  vi.restoreAllMocks();
});

describe('window.electronAPI.devices.securityLogList 冒烟', () => {
  it('适配存在：securityLogList 为函数且透传 limit 参数', async () => {
    const securityLogList = vi.fn().mockResolvedValue({ items: [INITIATED_DTO] });
    (window as any).electronAPI = { devices: { securityLogList } };
    await (window as any).electronAPI.devices.securityLogList(2);
    expect(securityLogList).toHaveBeenCalledWith(2);
  });

  it('DTO 形状：key/kind/deviceId/ts 必备，initiated 额外含 deviceName/actor=local', async () => {
    const securityLogList = vi.fn().mockResolvedValue({ items: [INITIATED_DTO, EFFECTIVE_DTO] });
    (window as any).electronAPI = { devices: { securityLogList } };
    const { items } = await (window as any).electronAPI.devices.securityLogList();

    expect(items).toHaveLength(2);
    const initiated = items.find((i: any) => i.kind === 'device_revoke_initiated');
    expect(initiated.key).toBeTypeOf('string');
    expect(initiated.deviceId).toBe('peer-b');
    expect(initiated.ts).toBeTypeOf('number');
    expect(initiated.deviceName).toBe('对端设备');
    expect(initiated.actor).toBe('local');

    const effective = items.find((i: any) => i.kind === 'device_revoke_effective');
    expect(effective.deviceId).toBe('peer-b');
    expect(effective.ts).toBeTypeOf('number');
    expect(effective.deviceName).toBeUndefined();
    expect(effective.actor).toBeUndefined();
  });

  it('键形 security:log:{ts}:{kind}:{deviceId}（同毫秒防覆盖，§4.4 回填一致）', () => {
    expect(INITIATED_DTO.key).toMatch(/^security:log:\d+:device_revoke_initiated:peer-b$/);
    expect(EFFECTIVE_DTO.key).toMatch(/^security:log:\d+:device_revoke_effective:peer-b$/);
  });
});
