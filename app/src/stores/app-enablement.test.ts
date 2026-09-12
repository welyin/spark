/**
 * app-enablement（per-space 启用事实源，install-and-enable §一/§二/§五）单测：
 * - 默认语义：内置件默认在 supportedSpaces 声明的空间启用；非内置默认未启用，
 *   个人空间记录缺席时回退内核全局开关（过渡种子）；
 * - 显式启停写 per-space 记录并持久化（spark:apps-enabled:<spaceKey>），覆盖默认值；
 * - 旧组织 mock（spark:apps-org-enabled，裸 orgId / org:<orgId> 两种历史键形）一次性迁移
 *   （首次显式启停落新键时，迁移值随记录一并保留，不丢失）；
 * - 空间隔离：同一插件在不同空间的启用记录互不影响。
 *
 * 注意：store 为模块级单例（含内存缓存），各用例使用互不相同的空间/插件 id 避免串扰。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
  defaultEnabledInSpace,
  enablementSpaceKey,
  isAppEnabledInSpace,
  isAppEnableableInSpace,
  isBuiltinApp,
  setAppEnabledInSpace
} from './app-enablement';
import type { EnablementAppRef } from './app-enablement';

const app = (id: string, over: Partial<EnablementAppRef> = {}): EnablementAppRef => ({
  id,
  supportedSpaces: ['personal', 'org'],
  enabled: false,
  ...over
});

const personal = { type: 'personal' } as const;
const space = (orgId: string) => ({ type: 'org', orgId } as const);

beforeEach(() => {
  localStorage.clear();
});

describe('app-enablement · 默认语义', () => {
  it('内置件默认在 supportedSpaces 声明的所有空间启用（组织成立即具备）', () => {
    const builtin = app('spark-chat');
    expect(isBuiltinApp('spark-chat')).toBe(true);
    expect(defaultEnabledInSpace(personal, builtin)).toBe(true);
    expect(isAppEnabledInSpace(personal, builtin)).toBe(true);
    expect(isAppEnabledInSpace(space('org-d1'), builtin)).toBe(true);
  });

  it('内置件 supportedSpaces 未声明的空间不默认启用；缺省声明按 [\'org\']', () => {
    expect(isAppEnabledInSpace(personal, app('spark-chat', { supportedSpaces: ['org'] }))).toBe(false);
    expect(isAppEnabledInSpace(space('org-d2'), app('spark-contacts', { supportedSpaces: undefined }))).toBe(true);
    expect(isAppEnabledInSpace(personal, app('spark-contacts', { supportedSpaces: undefined }))).toBe(false);
  });

  it('非内置：组织空间默认未启用；个人空间回退内核全局开关（过渡种子）', () => {
    expect(isAppEnabledInSpace(space('org-d3'), app('pd1', { enabled: true }))).toBe(false);
    expect(isAppEnabledInSpace(personal, app('pd1', { enabled: true }))).toBe(true);
    expect(isAppEnabledInSpace(personal, app('pd2', { enabled: false }))).toBe(false);
  });
});

describe('app-enablement · 显式启停', () => {
  it('写记录覆盖默认值并持久化到 spark:apps-enabled:<spaceKey>', () => {
    const org = space('org-w1');
    setAppEnabledInSpace(org, 'pw1', true);
    expect(isAppEnabledInSpace(org, app('pw1'))).toBe(true);
    expect(JSON.parse(localStorage.getItem('spark:apps-enabled:org:org-w1') ?? '{}')).toEqual({ pw1: true });

    // 显式停用覆盖内置默认启用
    setAppEnabledInSpace(org, 'spark-chat', false);
    expect(isAppEnabledInSpace(org, app('spark-chat'))).toBe(false);
  });

  it('空间隔离：org-w2 的启停不影响 personal / org-w3', () => {
    setAppEnabledInSpace(space('org-w2'), 'pw2', true);
    expect(isAppEnabledInSpace(space('org-w3'), app('pw2'))).toBe(false);
    expect(isAppEnabledInSpace(personal, app('pw2', { enabled: false }))).toBe(false);
  });
});

describe('app-enablement · 旧组织 mock 迁移', () => {
  it('新键缺席时读取旧 mock（裸 orgId 写入侧键形）', () => {
    localStorage.setItem('spark:apps-org-enabled:org-m1', JSON.stringify({ pm1: true }));
    expect(isAppEnabledInSpace(space('org-m1'), app('pm1'))).toBe(true);
  });

  it('新键缺席时读取旧 mock（org:<orgId> 读取侧键形），两种键形合并、读取侧优先', () => {
    localStorage.setItem('spark:apps-org-enabled:org:org-m2', JSON.stringify({ pm2: false }));
    localStorage.setItem('spark:apps-org-enabled:org-m2', JSON.stringify({ pm1: true, pm2: true }));
    const org = space('org-m2');
    expect(isAppEnabledInSpace(org, app('pm1'))).toBe(true);
    expect(isAppEnabledInSpace(org, app('pm2', { enabled: true }))).toBe(false);
  });

  it('迁移不丢数据：首次显式启停落新键时，迁移值随记录一并保留', () => {
    localStorage.setItem('spark:apps-org-enabled:org-m3', JSON.stringify({ pm3: true }));
    const org = space('org-m3');
    setAppEnabledInSpace(org, 'pm4', true);
    expect(isAppEnabledInSpace(org, app('pm4'))).toBe(true);
    // 迁移值不丢：旧 mock 的 pm3 仍在记录内
    expect(isAppEnabledInSpace(org, app('pm3'))).toBe(true);
    expect(JSON.parse(localStorage.getItem('spark:apps-enabled:org:org-m3') ?? '{}')).toEqual({ pm3: true, pm4: true });
  });
});

describe('app-enablement · 可启用域', () => {
  it('isAppEnableableInSpace：supportedSpaces 缺省按 [\'org\']', () => {
    expect(isAppEnableableInSpace(space('org-e1'), app('pe1', { supportedSpaces: undefined }))).toBe(true);
    expect(isAppEnableableInSpace(personal, app('pe1', { supportedSpaces: undefined }))).toBe(false);
    expect(isAppEnableableInSpace(personal, app('pe2', { supportedSpaces: ['personal'] }))).toBe(true);
  });

  it('enablementSpaceKey：personal / org:<orgId>（spaceKeyOf 同约定）', () => {
    expect(enablementSpaceKey(personal)).toBe('personal');
    expect(enablementSpaceKey(space('org-e2'))).toBe('org:org-e2');
  });
});
