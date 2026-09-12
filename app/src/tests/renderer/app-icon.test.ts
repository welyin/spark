/**
 * AppIcon 统一回退链渲染测试（plugin-dist §2.3）：
 * ① 包内图标（已安装且当前空间已启用，异步读包内 manifest.json 的 icon 相对路径，
 *    经插件源基址拼 URL）→ ② 声明图标（条目 icon，过 https/data:image 白名单）→
 * ③ 首字符 + 哈希渐变。覆盖：fail-closed（未安装/未启用不取包内图标）、声明图标
 * 白名单、manifest 缺席/路径非法回退、<img> 加载失败直落 ③。
 * jsdom 无真实图加载：包内图标断言 src 拼接，onerror 用派发的 error 事件模拟。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import AppIcon from '../../components/apps/AppIcon.vue';
import { sanitizePackageIconPath, type AppIconItem } from '../../components/apps/app-icon';
import { switchToPersonal } from '../../stores/current-space';
import { setAppEnabledInSpace } from '../../stores/app-enablement';

const PERSONAL = { type: 'personal' } as const;

/** 每个用例用独立插件 id：AppIcon 模块级 manifest 缓存跨用例共享，id 隔离防串扰 */
let seq = 0;
const nextId = () => `spark-icon-${++seq}`;

const makeItem = (id: string, over: Partial<AppIconItem> = {}): AppIconItem => ({
  id,
  name: '示例应用',
  icon: '',
  installed: false,
  enabled: false,
  supportedSpaces: ['personal', 'org'],
  ...over
});

const mountIcon = (item: AppIconItem | null, appId = ''): HTMLElement => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(AppIcon, { item, appId }) });
  app.mount(host);
  return host;
};

const flush = async () => {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
};

const stubManifestFetch = (manifest: unknown) => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({
      ok: true,
      json: async () => manifest
    }))
  );
};

beforeEach(() => {
  localStorage.clear();
  switchToPersonal();
});

afterEach(() => {
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('sanitizePackageIconPath（包内 icon 路径净化）', () => {
  it('接受常规相对路径并逐段编码', () => {
    expect(sanitizePackageIconPath('assets/icon.svg')).toBe('assets/icon.svg');
    expect(sanitizePackageIconPath('./assets/icon.svg')).toBe('assets/icon.svg');
  });

  it('拒绝空值 / 上级逃逸 / 盘符与 scheme；前导斜杠折叠为包内相对路径（与 plugin_src.rs 同口径）', () => {
    expect(sanitizePackageIconPath(undefined)).toBe('');
    expect(sanitizePackageIconPath('')).toBe('');
    expect(sanitizePackageIconPath('/assets/icon.svg')).toBe('assets/icon.svg');
    expect(sanitizePackageIconPath('../secret.svg')).toBe('');
    expect(sanitizePackageIconPath('assets/../../x.svg')).toBe('');
    expect(sanitizePackageIconPath('C:/x.svg')).toBe('');
    expect(sanitizePackageIconPath('https://evil.com/x.svg')).toBe('');
    expect(sanitizePackageIconPath('assets//icon.svg')).toBe('');
  });
});

describe('AppIcon 回退链（plugin-dist §2.3）', () => {
  it('② 未安装条目用声明图标（https URL），不触发包内 manifest 拉取', async () => {
    stubManifestFetch({ icon: 'assets/icon.svg' });
    const id = nextId();
    const host = mountIcon(makeItem(id, { icon: 'https://cdn.example.com/x.png' }));
    await flush();
    const img = host.querySelector('img');
    expect(img?.getAttribute('src')).toBe('https://cdn.example.com/x.png');
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it('② 声明图标过白名单：http:// 与 javascript: 一律不渲染，落回首字符+渐变', async () => {
    for (const bad of ['http://evil.com/x.png', 'javascript:alert(1)', 'file:///etc/passwd']) {
      const host = mountIcon(makeItem(nextId(), { icon: bad }));
      await flush();
      expect(host.querySelector('img')).toBeNull();
      expect(host.textContent).toBe('示');
      // 渐变底色为 inline style（jsdom 的 cssstyle 不解析 linear-gradient，此处不断言）
    }
  });

  it('① 已安装且当前空间已启用：异步解析包内 manifest icon，经插件源基址拼接', async () => {
    stubManifestFetch({ icon: 'assets/icon.svg' });
    const id = nextId();
    const host = mountIcon(makeItem(id, { installed: true, enabled: true }));
    // 挂载瞬间 manifest 未就位：先显示回退首字符
    expect(host.querySelector('img')).toBeNull();
    expect(host.textContent).toBe('示');
    await flush();
    const img = host.querySelector('img');
    // dev/浏览器链路：vite 中间件同 origin（pluginSourceBaseUrl 平台分支）
    expect(img?.getAttribute('src')).toBe(`${window.location.origin}/plugin/${id}/assets/icon.svg`);
  });

  it('① fail-closed：已安装但当前空间未启用，不取包内图标（回退声明/首字符）', async () => {
    stubManifestFetch({ icon: 'assets/icon.svg' });
    const id = nextId();
    // 显式停用（per-space 事实源）
    setAppEnabledInSpace(PERSONAL, id, false);
    const host = mountIcon(makeItem(id, { installed: true, enabled: true }));
    await flush();
    expect(host.querySelector('img')).toBeNull();
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
    expect(host.textContent).toBe('示');
  });

  it('① manifest 无 icon 字段 / 拉取失败：回退到声明图标，再回首字符', async () => {
    // manifest 无 icon → 声明图标
    stubManifestFetch({ id: nextId() });
    const id = nextId();
    const host = mountIcon(makeItem(id, { installed: true, enabled: true, icon: 'data:image/png;base64,AA==' }));
    await flush();
    expect(host.querySelector('img')?.getAttribute('src')).toBe('data:image/png;base64,AA==');

    // fetch 失败 → 首字符回退
    vi.stubGlobal('fetch', vi.fn(async () => { throw new Error('network down'); }));
    const id2 = nextId();
    const host2 = mountIcon(makeItem(id2, { installed: true, enabled: true }));
    await flush();
    expect(host2.querySelector('img')).toBeNull();
    expect(host2.textContent).toBe('示');
  });

  it('① manifest icon 路径非法（上级逃逸）：净化为空，走声明/回退', async () => {
    stubManifestFetch({ icon: '../../etc/passwd.svg' });
    const id = nextId();
    const host = mountIcon(makeItem(id, { installed: true, enabled: true }));
    await flush();
    expect(host.querySelector('img')).toBeNull();
    expect(host.textContent).toBe('示');
  });

  it('③ <img> 加载失败（onerror）直落首字符回退，不再回落声明图标', async () => {
    stubManifestFetch({ icon: 'assets/icon.svg' });
    const id = nextId();
    const host = mountIcon(
      makeItem(id, { installed: true, enabled: true, icon: 'https://cdn.example.com/fallback.png' })
    );
    await flush();
    const img = host.querySelector('img');
    expect(img?.getAttribute('src')).toContain(`/plugin/${id}/assets/icon.svg`);
    img!.dispatchEvent(new Event('error'));
    await nextTick();
    expect(host.querySelector('img')).toBeNull();
    expect(host.textContent).toBe('示');
  });

  it('无条目仅 appId：首字符回退（壳层内置窗口等无市场条目场景）', async () => {
    const host = mountIcon(null, 'spark:settings');
    await flush();
    expect(host.querySelector('img')).toBeNull();
    // 无名条目退到 id 首字符
    expect(host.textContent).toBe('s');
  });
});
