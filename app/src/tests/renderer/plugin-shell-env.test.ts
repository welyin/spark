/**
 * 壳层环境信息注入测试（A57，plugin/shell-env.ts）：
 * - 平台判定 userAgent 口径映射（与 plugin/source.ts 同源先例）；
 * - 握手 ctx 注入 appVersion / platform / shellVersion（壳层生成、只读下发）；
 * - appVersion 与应用包版本同源（不改入参，返回新对象）。
 */

import { describe, expect, it } from 'vitest';
import pkg from '../../../package.json';
import { resolvePluginPlatform, resolvePluginShellEnv, withPluginShellEnv } from '../../plugin/shell-env';
import type { PluginContext, PluginPlatform } from '../../../../packages/plugin-sdk/src';

const BASE_CTX: PluginContext = {
  pluginId: 'spark-example',
  viewId: 'default',
  domain: 'plugin:spark-example',
  space: { type: 'personal', id: 'personal' },
  theme: 'light',
  mount: { viewType: 'app' }
};

const PLATFORMS: PluginPlatform[] = ['windows', 'macos', 'linux', 'android', 'ios', 'unknown'];

describe('plugin/shell-env 平台判定', () => {
  it.each<[string, PluginPlatform]>([
    ['Mozilla/5.0 (Windows NT 10.0; Win64; x64)', 'windows'],
    ['Mozilla/5.0 (Linux; Android 13; Pixel 7)', 'android'],
    ['Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)', 'ios'],
    ['Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', 'macos'],
    ['Mozilla/5.0 (X11; Linux x86_64)', 'linux'],
    ['', 'unknown'],
    ['Mozilla/5.0 (jsdom)', 'unknown']
  ])('%s → %s', (userAgent, expected) => {
    expect(resolvePluginPlatform(userAgent)).toBe(expected);
  });
});

describe('plugin/shell-env 握手 ctx 注入', () => {
  it('注入 appVersion / platform / shellVersion（壳层生成，shellVersion 与握手 sdkVersion 同值）', () => {
    const ctx = withPluginShellEnv(BASE_CTX, '1');
    expect(ctx.appVersion).toBe(pkg.version);
    expect(ctx.shellVersion).toBe('1');
    expect(PLATFORMS).toContain(ctx.platform);
    // 契约版本单源：传入值原样进入 shellVersion，无独立硬编码
    expect(withPluginShellEnv(BASE_CTX, '2').shellVersion).toBe('2');
  });

  it('不改入参（返回新对象），原有 ctx 字段原样保留', () => {
    const ctx = withPluginShellEnv(BASE_CTX, '1');
    expect(ctx).not.toBe(BASE_CTX);
    expect(BASE_CTX.appVersion).toBeUndefined();
    expect(BASE_CTX.platform).toBeUndefined();
    expect(BASE_CTX.shellVersion).toBeUndefined();
    expect(ctx.pluginId).toBe('spark-example');
    expect(ctx.theme).toBe('light');
    expect(ctx.mount).toEqual({ viewType: 'app' });
  });

  it('环境信息快照：版本随包版本、platform 随当前 UA', () => {
    const env = resolvePluginShellEnv('1');
    expect(env.appVersion).toBe(pkg.version);
    expect(env.shellVersion).toBe('1');
    expect(env.platform).toBe(resolvePluginPlatform(navigator.userAgent));
  });
});
