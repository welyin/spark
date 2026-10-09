/**
 * 壳层 manifest 视图线形校验测试（A56：background 视图类型合法化）。
 *
 * 覆盖：
 * - 三种合法视图类型（app / message-card / background）正反例；
 * - background 视图与顶层 background 入口字段的配对规则（新线形强制、
 *   历史线形「仅入口无视图」兼容放行）；
 * - entryView 约束（必须存在于 views、不得指向 background 视图）；
 * - fetchPluginManifest 接线：非法 manifest 按「无 manifest」降级（null），
 *   合法 manifest 透传 background 入口字段（内核 QuickJS 对账读取的契约字段
 *   不丢失，后台启动链路不受校验影响）。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { PluginManifest } from '../../../../packages/plugin-sdk/src';

vi.mock('../../api', () => ({
  isTauri: () => true
}));

import { isPluginViewType, validatePluginManifest, PLUGIN_VIEW_TYPES } from '../../plugin/manifest';
import { fetchPluginManifest } from '../../plugin/source';

/** 最小合法 manifest 基底（只填本校验关心之外的必填字段占位） */
function baseManifest(overrides: Record<string, unknown>): PluginManifest {
  return {
    id: 'demo',
    domain: 'plugin:demo',
    entryView: 'main',
    views: [{ id: 'main', type: 'app', title: '主界面' }],
    ...overrides
  } as unknown as PluginManifest;
}

describe('isPluginViewType 白名单', () => {
  it('三种线形均合法', () => {
    expect(PLUGIN_VIEW_TYPES).toEqual(['app', 'message-card', 'background']);
    for (const type of PLUGIN_VIEW_TYPES) {
      expect(isPluginViewType(type)).toBe(true);
    }
  });

  it('未知类型与畸形值拒绝', () => {
    expect(isPluginViewType('panel')).toBe(false);
    expect(isPluginViewType('')).toBe(false);
    expect(isPluginViewType(undefined)).toBe(false);
    expect(isPluginViewType(42)).toBe(false);
  });
});

describe('validatePluginManifest 正例', () => {
  it('纯 app 视图', () => {
    expect(() => validatePluginManifest(baseManifest({}))).not.toThrow();
  });

  it('app + message-card（spark-example 线形）', () => {
    const manifest = baseManifest({
      views: [
        { id: 'main', type: 'app' },
        { id: 'post-card', type: 'message-card' }
      ]
    });
    expect(validatePluginManifest(manifest)).toBe(manifest);
  });

  it('background 视图 + 顶层 background 入口（A56 新线形）', () => {
    const manifest = baseManifest({
      views: [
        { id: 'main', type: 'app' },
        { id: 'bg', type: 'background' }
      ],
      background: 'views/background.js'
    });
    const result = validatePluginManifest(manifest);
    // 内核对账（plugin_runtime.rs）读取的契约字段必须原样透传
    expect(result.background).toBe('views/background.js');
  });

  it('顶层 background 入口而无 background 视图（ai-chat 历史线形）兼容放行', () => {
    const manifest = baseManifest({ background: 'views/background.js' });
    expect(() => validatePluginManifest(manifest)).not.toThrow();
  });

  it('未声明 entryView 不强制（类型上为必填但校验不扩大战线）', () => {
    const manifest = baseManifest({ entryView: undefined });
    expect(() => validatePluginManifest(manifest)).not.toThrow();
  });
});

describe('validatePluginManifest 反例', () => {
  it('未知视图类型拒绝', () => {
    const manifest = baseManifest({ views: [{ id: 'main', type: 'panel' }] });
    expect(() => validatePluginManifest(manifest)).toThrow(/InvalidManifest: view "main" has unknown type "panel"/);
  });

  it('background 视图缺顶层 background 入口拒绝（声明即空诺，fail-closed）', () => {
    const manifest = baseManifest({
      views: [
        { id: 'main', type: 'app' },
        { id: 'bg', type: 'background' }
      ]
    });
    expect(() => validatePluginManifest(manifest)).toThrow(/requires the top-level "background" entry field/);
  });

  it('顶层 background 入口为空串等同缺失', () => {
    const manifest = baseManifest({
      views: [{ id: 'bg', type: 'background' }],
      background: ''
    });
    expect(() => validatePluginManifest(manifest)).toThrow(/InvalidManifest/);
  });

  it('entryView 指向 background 视图拒绝（无 UI 面不可打开）', () => {
    const manifest = baseManifest({
      entryView: 'bg',
      views: [{ id: 'bg', type: 'background' }],
      background: 'views/background.js'
    });
    expect(() => validatePluginManifest(manifest)).toThrow(/must not be a background view/);
  });

  it('entryView 不在 views 中拒绝', () => {
    const manifest = baseManifest({ entryView: 'ghost' });
    expect(() => validatePluginManifest(manifest)).toThrow(/entryView "ghost" is not declared in views/);
  });

  it('views 非数组拒绝', () => {
    const manifest = baseManifest({ views: 'main' });
    expect(() => validatePluginManifest(manifest)).toThrow(/views must be an array/);
  });

  it('视图缺 id / id 空串拒绝', () => {
    expect(() => validatePluginManifest(baseManifest({ views: [{ type: 'app' }] }))).toThrow(/non-empty string id/);
    expect(() => validatePluginManifest(baseManifest({ views: [{ id: '', type: 'app' }] }))).toThrow(/non-empty string id/);
  });
});

describe('fetchPluginManifest 接线', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function stubFetch(payload: unknown, ok = true) {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok,
        json: async () => payload
      }))
    );
  }

  it('合法 manifest（含 background 视图新线形）原样返回，background 入口字段不丢失', async () => {
    const payload = {
      id: 'demo',
      domain: 'plugin:demo',
      entryView: 'main',
      views: [
        { id: 'main', type: 'app' },
        { id: 'bg', type: 'background' }
      ],
      background: 'views/background.js'
    };
    stubFetch(payload);
    const manifest = await fetchPluginManifest('demo');
    expect(manifest?.views).toHaveLength(2);
    expect(manifest?.background).toBe('views/background.js');
  });

  it('非法视图类型的 manifest 降级为 null（与读取失败同口径）', async () => {
    stubFetch({ id: 'demo', views: [{ id: 'main', type: 'panel' }] });
    expect(await fetchPluginManifest('demo')).toBeNull();
  });

  it('background 视图缺入口的 manifest 降级为 null', async () => {
    stubFetch({
      id: 'demo',
      entryView: 'main',
      views: [
        { id: 'main', type: 'app' },
        { id: 'bg', type: 'background' }
      ]
    });
    expect(await fetchPluginManifest('demo')).toBeNull();
  });

  it('HTTP 失败仍按原口径返回 null', async () => {
    stubFetch(null, false);
    expect(await fetchPluginManifest('demo')).toBeNull();
  });
});
