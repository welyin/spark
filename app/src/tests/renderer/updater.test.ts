// 主程序自动更新前端链路单测：
// - use-updater 就绪弹窗：updater://ready 回调 → ElMessageBox.confirm，
//   确认调 applyRestart、取消/关闭不调（状态保留，可去关于页手动安装）；
// - 无桥接（demo/浏览器）时静默不订阅、不报错；
// - 设置→关于：status() 回填版本与已就绪更新（出现「重启安装」按钮），
//   「检查更新」有更新时自动 stageLatest 并出现「重启安装」按钮。
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus, { ElMessage, ElMessageBox } from 'element-plus';
import SystemSettingsPanel from '../../components/settings/SystemSettingsPanel.vue';
import { useUpdaterReadyPrompt } from '../../components/updater/use-updater';
import type { UpdaterReadyInfo } from '../../api';

vi.mock('element-plus', async (importOriginal) => {
  const original = await importOriginal<typeof import('element-plus')>();
  return {
    ...original,
    ElMessage: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
    ElMessageBox: { ...original.ElMessageBox, confirm: vi.fn() }
  };
});

const confirmMock = ElMessageBox.confirm as unknown as ReturnType<typeof vi.fn>;
const messageErrorMock = ElMessage.error as unknown as ReturnType<typeof vi.fn>;

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

type UpdaterMock = {
  status: ReturnType<typeof vi.fn>;
  check: ReturnType<typeof vi.fn>;
  stageLatest: ReturnType<typeof vi.fn>;
  applyRestart: ReturnType<typeof vi.fn>;
  onReady: ReturnType<typeof vi.fn>;
};

function makeUpdater(overrides: Partial<UpdaterMock> = {}): UpdaterMock {
  return {
    status: vi.fn().mockResolvedValue({
      configured: true,
      appId: 'com.spark.desktop',
      channel: 'github-releases',
      currentVersion: '0.1.0',
      lastCheck: null,
      staged: null
    }),
    check: vi.fn().mockResolvedValue({ updateAvailable: false }),
    stageLatest: vi.fn(),
    applyRestart: vi.fn().mockResolvedValue(undefined),
    onReady: vi.fn().mockResolvedValue(() => {}),
    ...overrides
  };
}

function mountPromptHost(): { app: ReturnType<typeof createApp>; host: HTMLElement } {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({
    setup() {
      useUpdaterReadyPrompt();
      return () => h('div');
    }
  });
  app.mount(host);
  return { app, host };
}

describe('更新就绪弹窗（use-updater）', () => {
  beforeEach(() => {
    confirmMock.mockReset();
    messageErrorMock.mockReset();
    delete (window as any).electronAPI;
  });

  it('ready 事件弹确认框，确认后调 applyRestart', async () => {
    let readyCallback: ((info: UpdaterReadyInfo) => void) | undefined;
    const updater = makeUpdater({
      onReady: vi.fn((cb: (info: UpdaterReadyInfo) => void) => {
        readyCallback = cb;
        return Promise.resolve(() => {});
      })
    });
    (window as any).electronAPI = { updater };
    confirmMock.mockResolvedValue('confirm');

    mountPromptHost();
    await flush();
    expect(updater.onReady).toHaveBeenCalledTimes(1);

    readyCallback?.({ version: '0.2.0', notes: '修复若干问题' });
    expect(confirmMock).toHaveBeenCalledTimes(1);
    const [message, , options] = confirmMock.mock.calls[0];
    expect(message).toContain('0.2.0');
    expect(message).toContain('修复若干问题');
    expect(options.confirmButtonText).toBe('重启安装');

    await flush();
    expect(updater.applyRestart).toHaveBeenCalledTimes(1);
  });

  it('取消/关闭弹窗不调 applyRestart', async () => {
    let readyCallback: ((info: UpdaterReadyInfo) => void) | undefined;
    const updater = makeUpdater({
      onReady: vi.fn((cb: (info: UpdaterReadyInfo) => void) => {
        readyCallback = cb;
        return Promise.resolve(() => {});
      })
    });
    (window as any).electronAPI = { updater };
    confirmMock.mockRejectedValue('cancel');

    mountPromptHost();
    await flush();
    readyCallback?.({ version: '0.2.0' });
    await flush();

    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(updater.applyRestart).not.toHaveBeenCalled();
    expect(messageErrorMock).not.toHaveBeenCalled();
  });

  it('applyRestart 失败（非取消）提示错误', async () => {
    let readyCallback: ((info: UpdaterReadyInfo) => void) | undefined;
    const updater = makeUpdater({
      onReady: vi.fn((cb: (info: UpdaterReadyInfo) => void) => {
        readyCallback = cb;
        return Promise.resolve(() => {});
      }),
      applyRestart: vi.fn().mockRejectedValue(new Error('install failed'))
    });
    (window as any).electronAPI = { updater };
    confirmMock.mockResolvedValue('confirm');

    mountPromptHost();
    await flush();
    readyCallback?.({ version: '0.2.0' });
    await flush();
    await flush();

    expect(updater.applyRestart).toHaveBeenCalledTimes(1);
    expect(messageErrorMock).toHaveBeenCalledTimes(1);
    expect(String(messageErrorMock.mock.calls[0][0])).toContain('install failed');
  });

  it('无桥接时静默不订阅', async () => {
    expect(() => mountPromptHost()).not.toThrow();
    await flush();
    expect(confirmMock).not.toHaveBeenCalled();
  });
});

describe('设置→关于·手动更新', () => {
  beforeEach(() => {
    confirmMock.mockReset();
    delete (window as any).electronAPI;
  });

  function mountPanel(updater: UpdaterMock): HTMLElement {
    (window as any).electronAPI = {
      rootIdentity: {
        status: vi.fn().mockResolvedValue({
          initialized: true,
          unlocked: true,
          rootId: 'root-1',
          nickname: null,
          avatar: null
        })
      },
      p2p: {
        info: vi.fn().mockResolvedValue({
          initialized: true,
          started: false,
          peerId: null,
          addresses: [],
          connectedPeers: [],
          sparkSyncSubscribers: [],
          error: null
        })
      },
      dataManagement: {
        usage: vi.fn().mockResolvedValue({
          classes: {},
          totalKeys: 0,
          totalBytes: 0,
          warnings: { diskLow: false, usageExceeded: false }
        })
      },
      updater
    };
    const host = document.createElement('div');
    document.body.appendChild(host);
    const app = createApp({ render: () => h(SystemSettingsPanel) });
    app.use(ElementPlus);
    app.mount(host);
    return host;
  }

  function findButton(host: HTMLElement, text: string): HTMLButtonElement | undefined {
    return Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.includes(text));
  }

  async function openAboutSection(host: HTMLElement): Promise<void> {
    findButton(host, '关于')?.click();
    await flush();
  }

  it('status 回填版本与已就绪更新，出现「重启安装」按钮', async () => {
    const updater = makeUpdater({
      status: vi.fn().mockResolvedValue({
        configured: true,
        appId: 'com.spark.desktop',
        channel: 'github-releases',
        currentVersion: '0.1.0',
        lastCheck: null,
        staged: { fileName: 'Spark.app.tar.gz', version: '0.2.0' }
      })
    });
    const host = mountPanel(updater);
    await flush();
    await openAboutSection(host);

    expect(host.textContent).toContain('0.1.0');
    expect(host.textContent).toContain('新版本 0.2.0 已就绪');
    expect(findButton(host, '重启安装 0.2.0')).toBeTruthy();
    expect(updater.status).toHaveBeenCalledTimes(1);
  });

  it('检查更新：有更新自动下载并就绪，出现「重启安装」按钮', async () => {
    const updater = makeUpdater({
      check: vi.fn().mockResolvedValue({ updateAvailable: true, availableVersion: '0.2.0' }),
      stageLatest: vi.fn().mockResolvedValue({ fileName: 'Spark.app.tar.gz', version: '0.2.0' })
    });
    const host = mountPanel(updater);
    await flush();
    await openAboutSection(host);

    findButton(host, '检查更新')?.click();
    await flush();
    await flush();

    expect(updater.check).toHaveBeenCalledTimes(1);
    expect(updater.stageLatest).toHaveBeenCalledTimes(1);
    expect(findButton(host, '重启安装 0.2.0')).toBeTruthy();
    expect(host.textContent).toContain('点击「重启安装」完成更新');
  });

  it('检查更新：已是最新时不下载', async () => {
    const updater = makeUpdater();
    const host = mountPanel(updater);
    await flush();
    await openAboutSection(host);

    findButton(host, '检查更新')?.click();
    await flush();
    await flush();

    expect(updater.stageLatest).not.toHaveBeenCalled();
    expect(host.textContent).toContain('当前已是最新版本');
    expect(findButton(host, '重启安装')).toBeFalsy();
  });

  it('未配置更新源时「检查更新」按钮禁用', async () => {
    const updater = makeUpdater({
      status: vi.fn().mockResolvedValue({
        configured: false,
        appId: 'com.spark.desktop',
        channel: 'github-releases',
        currentVersion: '0.1.0',
        lastCheck: null,
        staged: null
      })
    });
    const host = mountPanel(updater);
    await flush();
    await openAboutSection(host);

    expect(findButton(host, '检查更新')?.disabled).toBe(true);
  });
});
