/**
 * 窗口化适配断言（PC 插件窗口化，ui-architecture §4.2；窗口最小夹取 320×220）。
 *
 * 议题墙为单列卡片流（无宽度断点），议题详情经 el-drawer 打开（应用内导航，
 * 保留）——断言聚焦结构不变量而非像素（jsdom 不做排版）：
 * 1. 三档视口下议题墙（头卡/发起表单[关闭规则网格]/关注表单/议题条目）结构完整，
 *    长身份 id 走 shortId 截断，无壳层重复元素；
 * 2. 详情抽屉尺寸随窗口形态：窄窗（320/480 <600）全宽 100%，宽窗（880）70%——
 *    固定 70% 在 320 窗口下抽屉仅 224px，时间线/表格不可用；
 * 3. 详情元信息 el-descriptions 窄窗单列堆叠、宽窗双栏（窗口 resize 即时联动）；
 * 4. 重复壳检查：本插件非沉浸式（manifest 未声明 chrome.hostTitleBar:false），
 *    视图内不得出现「关闭应用/返回桌面/退出」类壳元素，故不需要 moments 那样的
 *    (pointer:coarse) 退出钮显隐逻辑；
 * 5. 滚动契约：主视图/详情根节点不设 height/overflow，全文无 100vh——超高
 *    时间线由 iframe 原生文档滚动与 el-drawer 自带滚动体承载（源码静态断言，
 *    moments 修过的「overflow:hidden 父级裁切超高内容」bug 类在本插件不适用）。
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick, type App, type Component } from 'vue';
import ElementPlus from 'element-plus';
import type { PluginSDK } from '../../../packages/plugin-sdk/src';
import { AFFAIR_TYPE } from '../wire';

// ---------------------------------------------------------------------------
// mock 数据：一个关注的议题（创世 + 两条内容操作 + 一条决议 + 一阶名册条目）
// ---------------------------------------------------------------------------

const MS_PER_DAY = 86_400_000;
const NOW_MS = 1_700_000_000_000;
const PUB_KEY = '0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=';
const IDENTITY = '10ba682c8ad13513971e8b56881aab8bd702bb807796eca81932c735a94d6e6d';
const AFFAIR_ID = 'ab'.repeat(32);
const OP_HASH_1 = 'cd'.repeat(32);
const OP_HASH_2 = 'ef'.repeat(32);
const ORG_ID = `org_${'cd'.repeat(8)}`;

const GENESIS = {
  affairV: 1,
  type: AFFAIR_TYPE,
  title: '绿植补种',
  summary: '春天补种楼下花坛',
  tags: ['业委会', '预算'],
  initiator: { kind: 'person', identity: IDENTITY, publicKey: PUB_KEY },
  refs: [],
  createdAt: NOW_MS - 30 * MS_PER_DAY,
  rules: { engine: 'b1', ruleChange: { kind: 'delayed-veto', delayMs: MS_PER_DAY, vetoThreshold: { count: 1 } }, exec: null },
  sig: 'sig-1'
};

const OPS = [
  {
    opHash: OP_HASH_1,
    op: {
      actor: { kind: 'person', identity: IDENTITY },
      declaredAt: NOW_MS - 20 * MS_PER_DAY,
      prevOpHash: AFFAIR_ID,
      payload: { kind: 'contribution', text: '补种方案 v1：月季三株' }
    }
  },
  {
    opHash: OP_HASH_2,
    op: {
      actor: { kind: 'person', identity: IDENTITY },
      declaredAt: NOW_MS - 10 * MS_PER_DAY,
      prevOpHash: OP_HASH_1,
      payload: { kind: 'comment', text: '支持，预算从公共收益出' }
    }
  }
];

// ---------------------------------------------------------------------------
// mock SDK：按已落地 SDK 面（sdk-affairs REQUIRED_AFFAIRS_METHODS）钉住契约
// ---------------------------------------------------------------------------

function createMockSdk() {
  const affairs = {
    create: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, genesis: GENESIS }),
    follow: vi.fn().mockResolvedValue(AFFAIR_ID),
    unfollow: vi.fn().mockResolvedValue(undefined),
    listFollowed: vi.fn().mockResolvedValue([AFFAIR_ID]),
    submitOp: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, opHash: OP_HASH_2, status: 'accepted' }),
    readLog: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, genesis: GENESIS, ops: OPS, heads: [OP_HASH_2], followedAt: NOW_MS - 29 * MS_PER_DAY }),
    readRules: vi.fn().mockResolvedValue({
      affairId: AFFAIR_ID,
      nowMs: NOW_MS,
      current: { seq: 0, rulesHash: 'rh-0', rules: GENESIS.rules },
      versions: [{ seq: 0, basisOpHash: AFFAIR_ID, rulesHash: 'rh-0', effectiveMs: NOW_MS - 30 * MS_PER_DAY }],
      changes: []
    }),
    readResolution: vi.fn().mockResolvedValue({
      affairId: AFFAIR_ID,
      resolutions: [{ opHash: OP_HASH_1, state: 'pending', result: { text: '补种方案 v1' }, objections: 0, anchoredMs: null, pubPeriodMs: MS_PER_DAY }]
    }),
    ladderStatus: vi.fn().mockResolvedValue({
      affairId: AFFAIR_ID,
      nowMs: NOW_MS,
      entries: [{ identity: IDENTITY, tier: 'voter', accepts: 3, accountAgeMs: 30 * MS_PER_DAY, lastActivityMs: NOW_MS - 2 * MS_PER_DAY, tierSinceMs: NOW_MS - 15 * MS_PER_DAY }],
      voters: [IDENTITY]
    }),
    readExec: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, nowMs: NOW_MS, exec: null, states: [] }),
    orgEffects: vi.fn().mockResolvedValue({ orgId: ORG_ID, affairId: AFFAIR_ID, nowMs: NOW_MS, effects: [], invalidResolutions: [] }),
    applyOrgEffects: vi.fn().mockResolvedValue({ orgId: ORG_ID, affairId: AFFAIR_ID, nowMs: NOW_MS, actions: [], invalidResolutions: [] }),
    onChange: vi.fn().mockResolvedValue(undefined)
  };
  return {
    affairs,
    identity: {
      sign: vi.fn().mockResolvedValue({ domain: 'plugin:spark-affairs', domainId: 'spark-affairs', publicKey: PUB_KEY, signature: 'sig-1', payloadHash: 'ph-1' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    messages: { sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }) }
  } as unknown as PluginSDK;
}

// ---------------------------------------------------------------------------
// 挂载辅助
// ---------------------------------------------------------------------------

let mounted: Array<{ app: App; host: HTMLElement }> = [];
let AffairsView: Component;

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 等 onMounted 异步链（SDK 注入 → 关注列表加载）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mount(): Promise<HTMLElement> {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(AffairsView);
  app.use(ElementPlus);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

/** 点「打开」进议题详情抽屉，等抽屉内容与详情异步加载完成 */
async function openDetailDrawer(host: HTMLElement): Promise<void> {
  const openButton = [...host.querySelectorAll<HTMLButtonElement>('.affair-item .el-button')].find((b) => b.textContent?.includes('打开'));
  expect(openButton).not.toBeUndefined();
  openButton!.click();
  await flushMount();
}

beforeAll(async () => {
  // SDK 注入后再挂载（ensurePluginSDK 轮询 window.__sparkPluginSDK）
  (window as unknown as { __sparkPluginSDK: PluginSDK }).__sparkPluginSDK = createMockSdk();
  AffairsView = (await import('../AffairsView.vue')).default;
});

afterEach(() => {
  for (const { app, host } of mounted) {
    app.unmount();
    host.remove();
  }
  mounted = [];
});

describe('spark-affairs 窗口化布局三档断言', () => {
  it.each([320, 480, 880])('窗口宽 %i：议题墙头卡/发起表单/关注表单/议题条目结构完整，无壳层重复元素', async (width) => {
    setViewport(width);
    const host = await mount();

    // 头卡：标题 + 刷新钮
    expect(host.querySelector('.header-card h2')?.textContent).toBe('议题墙');
    // 发起表单：标题/简介/标签 + 关闭规则网格（阈值滑杆/法定人数/公示期/参与门槛四项）
    expect(host.querySelector('.composer-card')).not.toBeNull();
    expect(host.querySelectorAll('.rules-grid label')).toHaveLength(4);
    // 关注表单：创世记录粘贴区
    expect(host.textContent ?? '').toContain('关注已有议题');

    // 议题条目：标题 + 标签 + 长身份 id 截断 + 打开/取关钮
    expect(host.querySelector('.affair-meta strong')?.textContent).toBe('绿植补种');
    expect(host.querySelectorAll('.affair-meta .el-tag')).toHaveLength(2);
    expect(host.querySelector('.affair-origin')?.textContent).toContain(`发起人 ${IDENTITY.slice(0, 12)}…`);
    expect(host.querySelector('.affair-origin')?.textContent).not.toContain(IDENTITY);
    const actionTexts = [...host.querySelectorAll<HTMLButtonElement>('.affair-actions .el-button')].map((b) => b.textContent?.trim());
    expect(actionTexts).toEqual(['打开', '取关']);

    // 重复壳检查：无「关闭应用/返回桌面/退出」类壳元素（「关闭规则」是业务语义，不在此列）
    expect(host.textContent ?? '').not.toMatch(/关闭应用|返回桌面|退出/);
  });

  it.each([320, 480, 880])('窗口宽 %i：详情抽屉打开，元信息/名册/时间线/决议结构完整，抽屉尺寸与列数按形态', async (width) => {
    setViewport(width);
    const host = await mount();
    await openDetailDrawer(host);

    // 抽屉尺寸：窄窗全宽（320 下 70% 仅 224px 不可用），宽窗 70% 保留列表上下文
    const drawer = host.querySelector<HTMLElement>('.el-drawer');
    expect(drawer).not.toBeNull();
    expect(drawer!.style.width).toBe(width < 600 ? '100%' : '70%');

    // 元信息：窄窗单列（每行 2 格 = 1 项），宽窗双栏（每行 4 格 = 2 项）
    const firstRow = host.querySelector('.el-descriptions table tr');
    expect(firstRow).not.toBeNull();
    expect(firstRow!.children).toHaveLength(width < 600 ? 2 : 4);
    expect(host.textContent ?? '').toContain('零门槛（观察/评论）');

    // 阶梯名册（1 人投票者）+ 操作日志（2 条：贡献带投票三钮 + 评论）+ 决议（1 条公示中）
    expect(host.textContent ?? '').toContain('阶梯名册（1 人 · 投票者 1 人）');
    expect(host.querySelectorAll('.op-item').length).toBeGreaterThanOrEqual(2);
    expect(host.querySelectorAll('.op-actions .el-button')).toHaveLength(3);
    expect(host.textContent ?? '').toContain('决议（1）');
    expect(host.textContent ?? '').toContain('公示中');

    // 详情内同样无壳层重复元素
    expect(host.textContent ?? '').not.toMatch(/关闭应用|返回桌面|退出/);
  });

  it('窗口 resize 联动：880→480 抽屉 70%→100%、元信息双栏→单列；480→880 复原', async () => {
    setViewport(880);
    const host = await mount();
    await openDetailDrawer(host);
    expect(host.querySelector<HTMLElement>('.el-drawer')!.style.width).toBe('70%');
    expect(host.querySelector('.el-descriptions table tr')!.children).toHaveLength(4);

    setViewport(480);
    await flushMount();
    expect(host.querySelector<HTMLElement>('.el-drawer')!.style.width).toBe('100%');
    expect(host.querySelector('.el-descriptions table tr')!.children).toHaveLength(2);

    setViewport(880);
    await flushMount();
    expect(host.querySelector<HTMLElement>('.el-drawer')!.style.width).toBe('70%');
    expect(host.querySelector('.el-descriptions table tr')!.children).toHaveLength(4);
  });
});

describe('spark-affairs 滚动契约与 manifest 静态断言', () => {
  it('主视图/详情根节点不设 height/overflow，全文无 100vh（超高内容依赖 iframe 原生滚动与抽屉滚动体）', () => {
    // import.meta.url 在 vitest 下带 /@fs/ 前缀不可直接读，从工作目录（code/app）解析
    const view = readFileSync(resolve(process.cwd(), '../plugins/spark-affairs/AffairsView.vue'), 'utf8');
    expect(view).not.toMatch(/100vh/);
    const viewRoot = view.match(/\.spark-affairs\s*\{([^}]*)\}/);
    expect(viewRoot?.[1] ?? '').not.toMatch(/overflow|height/);

    const detail = readFileSync(resolve(process.cwd(), '../plugins/spark-affairs/AffairDetail.vue'), 'utf8');
    expect(detail).not.toMatch(/100vh/);
    const detailRoot = detail.match(/\.affair-detail\s*\{([^}]*)\}/);
    expect(detailRoot?.[1] ?? '').not.toMatch(/overflow|height/);
  });

  it('manifest 声明 window 默认尺寸 960×640（详情信息密度高，宽于壳层缺省 880×620）且在壳层合法范围内', () => {
    const manifest = JSON.parse(readFileSync(resolve(process.cwd(), '../plugins/spark-affairs/manifest.json'), 'utf8')) as {
      window?: { defaultWidth?: number; defaultHeight?: number };
    };
    // 与壳层 normalize_window 合法范围同口径（宽 320–3840 / 高 220–2160）
    expect(manifest.window).toEqual({ defaultWidth: 960, defaultHeight: 640 });
  });
});
