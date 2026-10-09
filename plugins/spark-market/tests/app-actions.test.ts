/**
 * spark-market 安装动作测试（app-actions.ts，移植自壳层 app-actions 的系统层子集）：
 * 权限确认门控（安装即授权，设计 §6.1）→ sdk.market.installFromRepo；
 * 网络差降级文案（仓库不可达提示侧载路径，plugin-dist §6 结构化前缀）。
 *
 * element-plus 对话框/提示打桩（jsdom 无真实 UI）；sdk.market 经 sdk-host
 * 打桩注入（组件树外的桥句柄）。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('element-plus', () => ({
  ElMessage: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), info: vi.fn() }),
  ElMessageBox: { confirm: vi.fn() }
}));

const marketApiMock = vi.fn();
vi.mock('../src/sdk-host', () => ({
  marketApi: () => marketApiMock()
}));

import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginMarketItem, PluginMarketRepoDeclaration } from '../../../packages/plugin-sdk/src';
import { confirmPermissions, installPluginItem, installRepoDeclaration } from '../src/app-actions';

const confirm = ElMessageBox.confirm as ReturnType<typeof vi.fn>;
const messageError = ElMessage.error as ReturnType<typeof vi.fn>;

function mkItem(overrides: Partial<PluginMarketItem> = {}): PluginMarketItem {
  return {
    id: 'github.com/acme/todo',
    domain: 'plugin:todo',
    name: '待办',
    icon: '',
    description: '任务管理',
    category: 'tool',
    version: '0.1.0',
    views: [],
    permissions: ['storage:read'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: '', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: null,
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: [],
    ...overrides
  };
}

function mkDeclaration(overrides: Partial<PluginMarketRepoDeclaration> = {}): PluginMarketRepoDeclaration {
  return {
    id: 'github.com/acme/todo',
    name: '待办',
    icon: '',
    summary: '任务管理',
    category: 'tool',
    version: '0.1.0',
    releaseAssetPattern: '',
    permissions: ['storage:read'],
    mirrors: [],
    sdkVersion: '1',
    ...overrides
  };
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe('confirmPermissions（安装即授权确认）', () => {
  it('无权限声明：不弹框直接放行', async () => {
    await expect(confirmPermissions('待办', [])).resolves.toBe(true);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('用户确认 → true；用户取消 → false', async () => {
    confirm.mockResolvedValueOnce({});
    await expect(confirmPermissions('待办', ['storage:read'])).resolves.toBe(true);
    expect(confirm).toHaveBeenCalledTimes(1);
    // 确认文案含权限中文名与原始码（授权知情）
    expect(confirm.mock.calls[0][0]).toContain('读取本域数据（storage:read）');

    confirm.mockRejectedValueOnce('cancel');
    await expect(confirmPermissions('待办', ['storage:read'])).resolves.toBe(false);
  });
});

describe('installPluginItem（权限确认 → installFromRepo）', () => {
  it('用户取消授权：不触达市场接口', async () => {
    const installFromRepo = vi.fn();
    marketApiMock.mockReturnValue({ installFromRepo });
    confirm.mockRejectedValueOnce('cancel');

    await expect(installPluginItem(mkItem())).resolves.toBe(false);
    expect(installFromRepo).not.toHaveBeenCalled();
  });

  it('授权后安装：installFromRepo 按条目 id 调用（目录驱动 install 已退役）', async () => {
    const installFromRepo = vi.fn(async () => null);
    marketApiMock.mockReturnValue({ installFromRepo });
    confirm.mockResolvedValueOnce({});

    await expect(installPluginItem(mkItem())).resolves.toBe(true);
    expect(installFromRepo).toHaveBeenCalledWith('github.com/acme/todo');
  });

  it('市场接口不可用（非插件运行上下文）：提示且返回 false', async () => {
    marketApiMock.mockReturnValue(undefined);
    confirm.mockResolvedValueOnce({});

    await expect(installPluginItem(mkItem())).resolves.toBe(false);
    expect(messageError).toHaveBeenCalledWith(expect.stringContaining('市场接口不可用'));
  });

  it('安装失败：错误如实上屏；仓库不可达降级为侧载指引（plugin-dist §6 前缀判定）', async () => {
    confirm.mockResolvedValue({});
    // 普通失败：原始错误串透传
    marketApiMock.mockReturnValue({
      installFromRepo: vi.fn(async () => {
        throw new Error('signature verify failed');
      })
    });
    await expect(installPluginItem(mkItem())).resolves.toBe(false);
    expect(messageError).toHaveBeenCalledWith(expect.stringContaining('signature verify failed'));

    // 仓库不可达：提示手动侧载路径
    marketApiMock.mockReturnValue({
      installFromRepo: vi.fn(async () => {
        throw new Error('Repo plugin github.com/acme/todo fetch failed: timeout');
      })
    });
    await expect(installPluginItem(mkItem())).resolves.toBe(false);
    expect(messageError).toHaveBeenCalledWith(expect.stringContaining('导入 .spkg 文件'));
  });
});

describe('installRepoDeclaration（仓库锚定安装）', () => {
  it('授权确认后按声明 id 安装', async () => {
    const installFromRepo = vi.fn(async () => null);
    marketApiMock.mockReturnValue({ installFromRepo });
    confirm.mockResolvedValueOnce({});

    await expect(installRepoDeclaration(mkDeclaration())).resolves.toBe(true);
    expect(installFromRepo).toHaveBeenCalledWith('github.com/acme/todo');
  });

  it('取消授权不安装', async () => {
    const installFromRepo = vi.fn();
    marketApiMock.mockReturnValue({ installFromRepo });
    confirm.mockRejectedValueOnce('cancel');

    await expect(installRepoDeclaration(mkDeclaration())).resolves.toBe(false);
    expect(installFromRepo).not.toHaveBeenCalled();
  });
});
