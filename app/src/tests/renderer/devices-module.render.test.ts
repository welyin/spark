// DevicesModule（设备管理）渲染回归（M2 撤销 + M1 新加入）。
//
// 覆盖方案文档 §6.15：
// - 列表行容器为 .mine-list-item（div，非 button）；
// - 已撤销行带 .device-revoked 类 + 唯一 el-tag(info)「已撤销」，无在线/可更新/
//   新加入 tag、无 .device-revoke-btn；
// - 撤销按钮 .device-revoke-btn 仅非本机且未撤销行显示；
// - 详情栏已撤销态 + 「撤销时间」行；
// - 确认对话框 ElMessageBox（teleport 到 body），标题「撤销设备『{name}』？」、
//   正文含「不会删除该设备上已有的数据」、确认钮「撤销」；
// - devices.revoke reject 'Cannot revoke current device' / 'Device not found' 走错误映射分支。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus, { ElMessage, ElMessageBox } from 'element-plus';
import DevicesModule from '../../components/mine/DevicesModule.vue';
import { setCurrentDevicePeerId } from '../../stores/device-notices';

type Device = {
  peerId: string;
  deviceName: string;
  os: string;
  osVersion: string;
  arch: string;
  macs: string[];
  appVersion: string;
  updatedAt: number;
  lastSeenAt: number;
  isSelf: boolean;
  online: boolean;
  revokedAt?: number | null;
};

const SELF: Device = {
  peerId: 'peer-self',
  deviceName: '我的电脑',
  os: 'Windows',
  osVersion: '10.0.22631',
  arch: 'x86_64',
  macs: [],
  appVersion: '1.5.0',
  updatedAt: 2000,
  lastSeenAt: 2000,
  isSelf: true,
  online: true
};

const HEALTHY: Device = {
  peerId: 'peer-phone',
  deviceName: '我的手机',
  os: 'Android',
  osVersion: '14',
  arch: 'aarch64',
  macs: [],
  appVersion: '1.5.0',
  updatedAt: 2000,
  lastSeenAt: 1500,
  isSelf: false,
  online: false
};

const REVOKED: Device = {
  peerId: 'peer-old-pc',
  deviceName: '旧电脑',
  os: 'Windows',
  osVersion: '10',
  arch: 'x86_64',
  macs: [],
  appVersion: '1.4.0',
  updatedAt: 1000,
  lastSeenAt: 1000,
  isSelf: false,
  online: false,
  revokedAt: 1700000000000
};

/** 挂载组件（覆盖 window.electronAPI.devices.list/revoke、updater.status）。 */
function mountApp(overrides: Partial<Record<'list' | 'revoke', ReturnType<typeof vi.fn>>>): {
  host: HTMLElement;
  list: ReturnType<typeof vi.fn>;
  revoke: ReturnType<typeof vi.fn>;
} {
  const list = overrides.list ?? vi.fn().mockResolvedValue([SELF, HEALTHY]);
  const revoke = overrides.revoke ?? vi.fn().mockResolvedValue({ success: true });
  (window as any).electronAPI = {
    devices: { list, revoke },
    updater: { status: vi.fn().mockResolvedValue(null) }
  };
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(DevicesModule, { rootId: 'root-render' }) });
  app.use(ElementPlus);
  app.mount(host);
  return { host, list, revoke };
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function rowByText(host: HTMLElement, text: string): HTMLElement {
  return Array.from(host.querySelectorAll('.mine-list-item')).find((r) =>
    (r as HTMLElement).textContent?.includes(text)
  ) as HTMLElement;
}

function revokeBtn(row: HTMLElement): HTMLElement | null {
  return row.querySelector('.device-revoke-btn');
}

beforeEach(() => {
  localStorage.clear();
  setCurrentDevicePeerId(null);
});

afterEach(() => {
  vi.restoreAllMocks();
  document.body.innerHTML = '';
});

describe('DevicesModule 设备列表渲染（M2）', () => {
  it('行容器为 div.mine-list-item（非 button）；撤销按钮仅非本机且未撤销行', async () => {
    const { host } = mountApp({});
    await flush();

    const rows = Array.from(host.querySelectorAll('.mine-list-item'));
    expect(rows.length).toBe(2);
    rows.forEach((row) => expect(row.tagName).toBe('DIV'));

    // 本机行：无撤销按钮。
    expect(revokeBtn(rowByText(host, '我的电脑'))).toBeNull();
    // 健康设备行：有撤销按钮。
    expect(revokeBtn(rowByText(host, '我的手机'))).not.toBeNull();
  });

  it('已撤销行：.device-revoked 类 + 唯一「已撤销」info tag，无在线/可更新/新加入/撤销按钮', async () => {
    const { host } = mountApp({ list: vi.fn().mockResolvedValue([SELF, REVOKED]) });
    await flush();

    const revokedRow = rowByText(host, '旧电脑');
    expect(revokedRow.classList.contains('device-revoked')).toBe(true);

    const revokedTags = Array.from(revokedRow.querySelectorAll('.el-tag')).filter((t) =>
      t.textContent?.includes('已撤销')
    );
    expect(revokedTags.length).toBe(1);
    expect((revokedTags[0] as HTMLElement).classList.contains('el-tag--info')).toBe(true);

    const text = revokedRow.textContent ?? '';
    expect(text).not.toContain('在线');
    expect(text).not.toContain('离线');
    expect(text).not.toContain('可更新');
    expect(text).not.toContain('新加入');
    expect(revokeBtn(revokedRow)).toBeNull();
  });

  it('详情栏已撤销态：显示「已撤销」标签 + 「撤销时间」行', async () => {
    const { host } = mountApp({ list: vi.fn().mockResolvedValue([SELF, REVOKED]) });
    await flush();

    rowByText(host, '旧电脑').click();
    await flush();

    const detail = host.querySelector('.panel-card') as HTMLElement;
    expect(detail).not.toBeNull();
    expect(detail.textContent).toContain('已撤销');
    const revokedRowEl = Array.from(detail.querySelectorAll('.device-row')).find((r) =>
      (r.querySelector('.device-row-label') as HTMLElement)?.textContent === '撤销时间'
    ) as HTMLElement;
    expect(revokedRowEl).not.toBeUndefined();
    const value = (revokedRowEl.querySelector('.device-row-value') as HTMLElement).textContent ?? '';
    expect(value).not.toBe('—');
  });
});

describe('DevicesModule 撤销确认与错误映射（M2）', () => {
  it('点击撤销弹出 ElMessageBox 确认框：标题/正文/确认钮逐字', async () => {
    const confirmMock = vi
      .spyOn(ElMessageBox, 'confirm')
      .mockResolvedValue('confirm' as never);
    const { host } = mountApp({});
    await flush();

    revokeBtn(rowByText(host, '我的手机'))!.click();
    await flush();

    expect(confirmMock).toHaveBeenCalledTimes(1);
    const [message, title, options] = confirmMock.mock.calls[0] as unknown as [
      string,
      string,
      Record<string, string>
    ];
    expect(title).toBe('撤销设备『我的手机』？');
    expect(message).toContain('不会删除该设备上已有的数据');
    expect(options.confirmButtonText).toBe('撤销');
    expect(options.type).toBe('warning');
  });

  it('确认后调用 devices.revoke(peerId)', async () => {
    vi.spyOn(ElMessageBox, 'confirm').mockResolvedValue('confirm' as never);
    const { host, revoke } = mountApp({});
    await flush();

    revokeBtn(rowByText(host, '我的手机'))!.click();
    await flush();
    expect(revoke).toHaveBeenCalledWith('peer-phone');
  });

  it("revoke reject 'Cannot revoke current device' → 错误映射：提示本机锁定", async () => {
    vi.spyOn(ElMessageBox, 'confirm').mockResolvedValue('confirm' as never);
    const errorSpy = vi
      .spyOn(ElMessage, 'error')
      .mockImplementation(() => ({}) as never);
    const mounted = mountApp({
      revoke: vi
        .fn()
        .mockRejectedValue(new Error("Error invoking remote method 'root-revoke-device': Error: Cannot revoke current device"))
    });
    await flush();
    revokeBtn(rowByText(mounted.host, '我的手机'))!.click();
    await flush();
    expect(errorSpy).toHaveBeenCalledWith('不能撤销当前设备：本机请使用「锁定设备」');
  });

  it("revoke reject 'Device not found' → 错误映射分支", async () => {
    vi.spyOn(ElMessageBox, 'confirm').mockResolvedValue('confirm' as never);
    const errorSpy = vi
      .spyOn(ElMessage, 'error')
      .mockImplementation(() => ({}) as never);
    const mounted = mountApp({ revoke: vi.fn().mockRejectedValue(new Error('Error: Device not found')) });
    await flush();
    revokeBtn(rowByText(mounted.host, '我的手机'))!.click();
    await flush();
    expect(errorSpy).toHaveBeenCalledWith('设备不存在或已被移除');
  });
});
