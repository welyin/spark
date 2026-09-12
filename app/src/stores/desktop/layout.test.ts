/**
 * 桌面布局（D6 排列方式）回归：
 * 排列方式属本机偏好、按空间本地记忆（spark:desktop:<spaceId>），缺省自由放置；
 * 切换即时生效并持久化；自由坐标在模式切换间保留（切回不丢）。
 * 注：currentSpace mock 为 personal，存储键恒 spark:desktop:personal。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../current-space', async () => {
  const { ref } = await import('vue');
  return { currentSpace: ref({ type: 'personal' }) };
});

const STORAGE_KEY = 'spark:desktop:personal';

/** 每用例拿到全新模块（layouts 为模块级缓存），以便覆盖 localStorage 装载路径 */
async function loadLayout() {
  vi.resetModules();
  return await import('./layout');
}

beforeEach(() => {
  localStorage.clear();
});

describe('desktop layout · D6 排列方式', () => {
  it('缺省为自由放置（free），保持现状绝对坐标语义', async () => {
    const { arrangeMode, desktopLayout } = await loadLayout();
    expect(arrangeMode.value).toBe('free');
    expect(desktopLayout.value.arrange).toBe('free');
  });

  it('setArrangeMode 切换即时生效，并按空间持久化到 localStorage', async () => {
    const { arrangeMode, setArrangeMode } = await loadLayout();
    setArrangeMode('grid');
    expect(arrangeMode.value).toBe('grid');
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}');
    expect(stored.arrange).toBe('grid');
    setArrangeMode('free');
    expect(arrangeMode.value).toBe('free');
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}').arrange).toBe('free');
  });

  it('重启后从本机存储恢复；非法值回退 free', async () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ arrange: 'grid' }));
    let mod = await loadLayout();
    expect(mod.arrangeMode.value).toBe('grid');

    localStorage.setItem(STORAGE_KEY, JSON.stringify({ arrange: 'weird' }));
    mod = await loadLayout();
    expect(mod.arrangeMode.value).toBe('free');
  });

  it('切换排列方式保留自由坐标（切回不丢）', async () => {
    const { desktopLayout, saveIconPosition, setArrangeMode } = await loadLayout();
    saveIconPosition('app-a', { x: 100, y: 200 });
    setArrangeMode('grid');
    setArrangeMode('free');
    expect(desktopLayout.value.positions?.['app-a']).toEqual({ x: 100, y: 200 });
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}');
    expect(stored.positions['app-a']).toEqual({ x: 100, y: 200 });
  });
});

describe('desktop layout · D16 每空间主题色', () => {
  it('缺省无自定义主题色（由桌面层按 spaceId 派生）', async () => {
    const { desktopLayout } = await loadLayout();
    expect(desktopLayout.value.themeColor).toBeUndefined();
  });

  it('saveThemeColor 写入并持久化；传 null 恢复派生默认', async () => {
    const { desktopLayout, saveThemeColor } = await loadLayout();
    saveThemeColor('#3296fa');
    expect(desktopLayout.value.themeColor).toBe('#3296fa');
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}').themeColor).toBe('#3296fa');
    saveThemeColor(null);
    expect(desktopLayout.value.themeColor).toBeUndefined();
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}').themeColor).toBeUndefined();
  });

  it('重启后从本机存储恢复；非字符串值丢弃', async () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ themeColor: '#7b61ff' }));
    let mod = await loadLayout();
    expect(mod.desktopLayout.value.themeColor).toBe('#7b61ff');

    localStorage.setItem(STORAGE_KEY, JSON.stringify({ themeColor: 42 }));
    mod = await loadLayout();
    expect(mod.desktopLayout.value.themeColor).toBeUndefined();
  });
});
