/**
 * 应用市场 mock 数据（ui-apps-market）。
 *
 * 真实市场（window.electronAPI.pluginMarket）目前只有 spark-example 一个插件，
 * 列表/市场/分组 UI 太空，这里伪造若干应用填充展示。mock 条目与真实
 * pluginMarket.list() 结果合并展示；安装/启用状态存 localStorage，
 * 「打开」不进入真实插件链路（占位 toast）。待市场数据充足后整体删除本文件。
 */
import type { PluginMarketItemDto } from '../api/types';

// TODO(mock): 伪造市场应用（真实市场仅 spark-example），待市场数据充足后删除
const MOCK_APPS: PluginMarketItemDto[] = [
  {
    id: 'mock-forum',
    domain: 'mock.forum',
    name: '论坛',
    description: '组织内部讨论区：发帖、回帖、按版块浏览，支持置顶与精华。',
    category: 'business',
    version: '1.2.0',
    views: ['default'],
    permissions: ['storage:read', 'storage:write', 'org:read'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: 'mock-forum', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: '1.2.0',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  },
  {
    id: 'mock-vote',
    domain: 'mock.vote',
    name: '投票',
    description: '快速发起单选/多选投票，实时统计结果，支持匿名与截止提醒。',
    category: 'business',
    version: '0.9.1',
    views: ['default'],
    permissions: ['storage:read', 'storage:write'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: 'mock-vote', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: '0.9.1',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  },
  {
    id: 'mock-calendar',
    domain: 'mock.calendar',
    name: '日历',
    description: '团队共享日历：会议排期、订阅组织日程、冲突检测与提醒。',
    category: 'business',
    version: '2.0.3',
    views: ['default'],
    permissions: ['storage:read', 'storage:write', 'org:read', 'org:sync'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: 'mock-calendar', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: '2.0.3',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  },
  {
    id: 'mock-kanban',
    domain: 'mock.kanban',
    name: '任务看板',
    description: '拖拽式任务看板：自定义泳道、负责人与截止日期，进度一目了然。',
    category: 'business',
    version: '1.5.0',
    views: ['default'],
    permissions: ['storage:read', 'storage:write', 'org:sync'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: 'mock-kanban', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: '1.5.0',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  },
  {
    id: 'mock-moments',
    domain: 'mock.moments',
    name: '朋友圈',
    description: '团队动态圈：分享图文动态，点赞与评论，仅组织成员可见。',
    category: 'business',
    version: '1.0.6',
    views: ['default'],
    permissions: ['storage:read', 'storage:write', 'org:read'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: 'mock-moments', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: '1.0.6',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  },
  {
    id: 'mock-files',
    domain: 'mock.files',
    name: '文件',
    description: '团队文件柜：目录共享、版本历史与在线预览，断点续传。',
    category: 'business',
    version: '3.1.2',
    views: ['default'],
    permissions: ['storage:read', 'storage:write', 'org:sync', 'network:broadcast'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: 'mock-files', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: '3.1.2',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  }
];

type MockAppState = { installed: boolean; enabled: boolean };

// TODO(mock): mock 应用的安装/启用状态存 localStorage，装了就出现在已安装列表；待真实市场接口替换
const STORAGE_KEY = 'spark:mock-apps-state';

/** 默认预装两个应用，让「已安装」列表与分组开箱有内容 */
const DEFAULT_STATE: Record<string, MockAppState> = {
  'mock-calendar': { installed: true, enabled: true },
  'mock-kanban': { installed: true, enabled: true }
};

function loadState(): Record<string, MockAppState> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      return { ...DEFAULT_STATE, ...(JSON.parse(raw) as Record<string, MockAppState>) };
    }
  } catch {
    // 本地存储不可读/数据损坏时按默认状态处理
  }
  return { ...DEFAULT_STATE };
}

function saveState(state: Record<string, MockAppState>): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // 持久化失败不阻断交互（重启后按默认状态）
  }
}

/** 判断市场条目是否为 mock 应用（mock 项不走真实安装/打开链路） */
export function isMockApp(item: PluginMarketItemDto): boolean {
  return item.id.startsWith('mock-');
}

/** mock 应用列表（叠加 localStorage 中的安装/启用状态），与真实市场列表合并展示 */
export function listMockApps(): PluginMarketItemDto[] {
  const state = loadState();
  return MOCK_APPS.map((app) => {
    const saved = state[app.id] ?? { installed: false, enabled: false };
    return {
      ...app,
      installed: saved.installed,
      enabled: saved.installed && saved.enabled,
      installedVersion: saved.installed ? app.version : null
    };
  });
}

export function setMockAppInstalled(pluginId: string, installed: boolean): void {
  const state = loadState();
  const current = state[pluginId] ?? { installed: false, enabled: false };
  state[pluginId] = { installed, enabled: installed ? current.enabled : false };
  saveState(state);
}

export function setMockAppEnabled(pluginId: string, enabled: boolean): void {
  const state = loadState();
  const current = state[pluginId] ?? { installed: false, enabled: false };
  state[pluginId] = { ...current, enabled };
  saveState(state);
}
