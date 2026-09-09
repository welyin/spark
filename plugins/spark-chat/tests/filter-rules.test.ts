/**
 * 个人过滤规则单测（communication §4.3，product/todo #22①）：
 * - CRUD + declareCollection 持久化（fake sdk.data 后端 = 插件数据域；
 *   集合 scope:"local" = 数据不离开本机，wiki plugin-data-api §2）；
 * - 匹配语义：关键词大小写不敏感子串 / 来源 senderId 精确匹配 / 多规则或语义
 *   组合；禁用规则不生效；自己发出的消息不过滤；
 * - 导入导出：导出 → 清空 → 导入还原；同 id 合并不覆盖本地；畸形导入
 *   逐类拒绝（不崩、不改动现有规则）；
 * - 未绑定 SDK 退化为纯内存（vitest / 纯前端预览）。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import type { PluginContext, PluginDataAPI, PluginSDK } from '../../../packages/plugin-sdk/src';
import { bindPluginRuntime } from '../src/sdk-host';
import {
  FILTER_RULES_COLLECTION,
  addKeywordRule,
  addSourceRule,
  clearFilterRules,
  exportFilterRules,
  importFilterRules,
  isMessageFiltered,
  listFilterRules,
  parseFilterRulesImport,
  removeFilterRule,
  resetFilterRulesCache,
  setRuleEnabled,
  type FilterRule
} from '../src/filter-rules';

function fakeCtx(): PluginContext {
  return {
    pluginId: 'spark-chat',
    viewId: 'default',
    domain: 'plugin:spark-chat',
    space: { type: 'personal', id: 'personal' },
    theme: 'light',
    mount: { viewType: 'app' }
  };
}

/** 内存后端：fake sdk.data 背后持有的「插件数据域」 */
function fakeDataBackend() {
  const store = new Map<string, unknown>();
  const declarations: Array<Record<string, unknown>> = [];
  const api = {
    declareCollection: (decl: Record<string, unknown>) => {
      declarations.push(decl);
      return Promise.resolve({});
    },
    save: (name: string, key: string, value: unknown) => {
      store.set(`${name}\n${key}`, value);
      return Promise.resolve({ success: true });
    },
    delete: (name: string, key: string) => {
      store.delete(`${name}\n${key}`);
      return Promise.resolve({ success: true });
    },
    query: (name: string) =>
      Promise.resolve({
        items: [...store.entries()]
          .filter(([k]) => k.startsWith(`${name}\n`))
          .map(([k, value]) => ({ key: k.slice(name.length + 1), value }))
      })
  } as unknown as PluginDataAPI;
  return { store, declarations, api };
}

/** 绑 fake 后端并清缓存（模块级单例跨用例共享，等价登录态切换语义） */
function bind(backend: ReturnType<typeof fakeDataBackend>): void {
  bindPluginRuntime({ data: backend.api } as unknown as PluginSDK, fakeCtx());
  resetFilterRulesCache();
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  bindPluginRuntime(null as unknown as PluginSDK, fakeCtx());
  resetFilterRulesCache();
});

describe('CRUD + declareCollection 持久化', () => {
  it('新增关键词/来源规则：本地立即可见，持久化直通 sdk.data.save', async () => {
    const backend = fakeDataBackend();
    bind(backend);
    const kw = addKeywordRule(' 广告 ');
    const src = addSourceRule('peer-spam');
    expect(kw?.target).toBe('广告'); // 首尾空白裁剪
    expect(src?.kind).toBe('source');
    expect(listFilterRules().map((r) => r.id)).toEqual([kw?.id, src?.id]);
    await flush();
    expect(backend.store.get(`${FILTER_RULES_COLLECTION}\n${kw?.id}`)).toMatchObject({ kind: 'keyword', target: '广告', enabled: true });
    expect(backend.store.get(`${FILTER_RULES_COLLECTION}\n${src?.id}`)).toMatchObject({ kind: 'source', target: 'peer-spam' });
  });

  it('同 kind+target 幂等不重复建；空目标拒绝', () => {
    bind(fakeDataBackend());
    const first = addKeywordRule('广告');
    const dup = addKeywordRule('广告');
    expect(dup?.id).toBe(first?.id);
    expect(addKeywordRule('   ')).toBeUndefined();
    expect(addSourceRule('')).toBeUndefined();
    expect(listFilterRules()).toHaveLength(1);
  });

  it('启停/删除/清空：本地态与持久化同步', async () => {
    const backend = fakeDataBackend();
    bind(backend);
    const kw = addKeywordRule('广告')!;
    const src = addSourceRule('peer-spam')!;
    setRuleEnabled(kw.id, false);
    await flush();
    expect(backend.store.get(`${FILTER_RULES_COLLECTION}\n${kw.id}`)).toMatchObject({ enabled: false });

    removeFilterRule(src.id);
    expect(listFilterRules().map((r) => r.id)).toEqual([kw.id]);
    await flush();
    expect(backend.store.has(`${FILTER_RULES_COLLECTION}\n${src.id}`)).toBe(false);

    clearFilterRules();
    expect(listFilterRules()).toEqual([]);
    await flush();
    expect([...backend.store.keys()].filter((k) => k.startsWith(FILTER_RULES_COLLECTION))).toEqual([]);
  });

  it('首次访问水合：声明集合（scope local）后读回持久化规则，坏项跳过不崩', async () => {
    const backend = fakeDataBackend();
    const persisted: FilterRule = { id: 'fr-seed-1', kind: 'keyword', target: '代购', enabled: true, createdAt: 1_000 };
    backend.store.set(`${FILTER_RULES_COLLECTION}\n${persisted.id}`, persisted);
    backend.store.set(`${FILTER_RULES_COLLECTION}\nfr-broken`, { kind: 'keyword' }); // 结构非法
    bind(backend);

    expect(listFilterRules()).toEqual([]); // 同步返回缓存（水合前为空）
    await flush();
    expect(backend.declarations).toEqual([{ name: FILTER_RULES_COLLECTION, scope: 'local' }]);
    expect(listFilterRules()).toEqual([persisted]);
  });
});

describe('渲染前过滤匹配（isMessageFiltered）', () => {
  it('关键词命中：大小写不敏感子串', () => {
    bind(fakeDataBackend());
    addKeywordRule('ads');
    expect(isMessageFiltered({ senderId: 'peer-1', content: 'ADS 大促销' })).toBe(true);
    expect(isMessageFiltered({ senderId: 'peer-1', content: '正常聊天' })).toBe(false);
  });

  it('来源命中：senderId 精确匹配', () => {
    bind(fakeDataBackend());
    addSourceRule('peer-spam');
    expect(isMessageFiltered({ senderId: 'peer-spam', content: '任何内容' })).toBe(true);
    expect(isMessageFiltered({ senderId: 'peer-spam-2', content: '任何内容' })).toBe(false);
  });

  it('组合规则：关键词 + 来源或语义，叠加越多过滤越宽；禁用规则不生效', () => {
    bind(fakeDataBackend());
    const kw = addKeywordRule('广告')!;
    addSourceRule('peer-spam');
    expect(isMessageFiltered({ senderId: 'peer-1', content: '这是广告' })).toBe(true);
    expect(isMessageFiltered({ senderId: 'peer-spam', content: '正常内容' })).toBe(true);
    expect(isMessageFiltered({ senderId: 'peer-1', content: '正常内容' })).toBe(false);

    setRuleEnabled(kw.id, false);
    expect(isMessageFiltered({ senderId: 'peer-1', content: '这是广告' })).toBe(false);
    expect(isMessageFiltered({ senderId: 'peer-spam', content: '正常内容' })).toBe(true);
  });

  it('自己发出的消息不过滤（否则用户看不见自己刚发出的内容）', () => {
    bind(fakeDataBackend());
    addKeywordRule('广告');
    expect(isMessageFiltered({ senderId: 'me', content: '我也提到广告这个词' })).toBe(false);
  });
});

describe('导入导出（换插件可携带）', () => {
  it('导出 → 清空 → 导入还原（同 id 同内容）', async () => {
    const backend = fakeDataBackend();
    bind(backend);
    addKeywordRule('广告');
    addSourceRule('peer-spam');
    const exported = exportFilterRules();
    expect(JSON.parse(exported).format).toBe('spark-chat-filter-rules');

    clearFilterRules();
    expect(listFilterRules()).toEqual([]);

    const result = importFilterRules(exported);
    expect(result).toEqual({ ok: true, imported: 2, skipped: 0 });
    expect(listFilterRules().map((r) => `${r.kind}:${r.target}`).sort()).toEqual(['keyword:广告', 'source:peer-spam']);
    await flush();
    // 还原后规则重新落插件数据域
    expect([...backend.store.keys()].filter((k) => k.startsWith(FILTER_RULES_COLLECTION))).toHaveLength(2);
  });

  it('同 id 合并不覆盖本地（本地状态优先）', () => {
    bind(fakeDataBackend());
    const local = addKeywordRule('广告')!;
    const exported = JSON.stringify({
      format: 'spark-chat-filter-rules',
      version: 1,
      rules: [
        { ...local, target: '被篡改的目标' },
        { id: 'fr-new-1', kind: 'source', target: 'peer-x', enabled: true, createdAt: 2_000 }
      ]
    });
    const result = importFilterRules(exported);
    expect(result).toEqual({ ok: true, imported: 1, skipped: 1 });
    expect(listFilterRules().find((r) => r.id === local.id)?.target).toBe('广告');
    expect(listFilterRules().some((r) => r.id === 'fr-new-1')).toBe(true);
  });

  it('畸形导入逐类拒绝：不崩、不改动现有规则', () => {
    bind(fakeDataBackend());
    addKeywordRule('广告');
    const before = listFilterRules();

    const cases: Array<[string, string, RegExp]> = [
      ['非 JSON', 'not json at all', /JSON/],
      ['非对象', '"just a string"', /对象/],
      ['format 不匹配', JSON.stringify({ format: 'other', version: 1, rules: [] }), /format/],
      ['版本不支持', JSON.stringify({ format: 'spark-chat-filter-rules', version: 99, rules: [] }), /版本/],
      ['缺 rules 数组', JSON.stringify({ format: 'spark-chat-filter-rules', version: 1 }), /rules/],
      [
        '规则项结构非法',
        JSON.stringify({ format: 'spark-chat-filter-rules', version: 1, rules: [{ id: 'x', kind: 'bogus', target: 't', enabled: true, createdAt: 1 }] }),
        /非法/
      ],
      [
        '规则 id 重复',
        JSON.stringify({
          format: 'spark-chat-filter-rules',
          version: 1,
          rules: [
            { id: 'x', kind: 'keyword', target: 'a', enabled: true, createdAt: 1 },
            { id: 'x', kind: 'keyword', target: 'b', enabled: true, createdAt: 2 }
          ]
        }),
        /重复/
      ]
    ];
    for (const [label, text, reason] of cases) {
      const parsed = parseFilterRulesImport(text);
      expect(parsed.ok, label).toBe(false);
      if (!parsed.ok) expect(parsed.reason, label).toMatch(reason);
      const result = importFilterRules(text);
      expect(result.ok, label).toBe(false);
    }
    // 全部拒绝后现有规则原样还在
    expect(listFilterRules()).toEqual(before);
  });
});

describe('未绑定 SDK（vitest / 纯前端预览）', () => {
  it('退化为纯内存：CRUD 可用、不发任何调用、不抛错', () => {
    // beforeEach 已绑定 null SDK
    addKeywordRule('广告');
    expect(isMessageFiltered({ senderId: 'peer-1', content: '广告位招租' })).toBe(true);
    clearFilterRules();
    expect(listFilterRules()).toEqual([]);
  });
});
