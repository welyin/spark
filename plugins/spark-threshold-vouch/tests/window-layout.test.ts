/**
 * 窗口化适配断言（PC 插件窗口化，ui-architecture §4.2；窗口最小夹取 320×220）。
 *
 * 主视图为单列卡片流（头卡/发起表单/担保请求列表/门槛证明列表纵向堆叠、
 * 无宽度断点），320/480/880 三档同构——断言聚焦结构不变量而非像素
 * （jsdom 不做排版）：
 * 1. 三档视口下头卡/发起表单/请求条目（担保输入 + 签名担保 + 组装证明）/
 *    证明条目结构完整，长 rootId（自报文本）全文渲染、折行样式静态锁定；
 * 2. 验签（免权限）展开逐项检查结论，检查项名含长担保人 id 仍完整渲染；
 * 3. 重复壳检查：本插件非沉浸式（manifest 未声明 chrome.hostTitleBar:false，
 *    移动全屏由壳层顶栏提供返回），视图内不得出现「关闭应用/返回桌面/退出」
 *    类壳元素，故不需要 moments 那样的 (pointer:coarse) 退出钮显隐逻辑；
 * 4. 滚动契约：主视图根节点不设 height/overflow，全文无 100vh——超高请求/
 *    证明列表由 iframe 原生文档滚动承载（源码静态断言，moments 修过的
 *    「overflow:hidden 父级裁切超高内容」bug 类在本插件不适用）；
 * 5. 窄窗适配样式静态锁定：操作行 flex-wrap（担保输入 + 两按钮换行不溢出）、
 *    长 rootId/context 任意断行（overflow-wrap）——jsdom 不排版，锁规则存在。
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick, type App, type Component } from 'vue';
import ElementPlus from 'element-plus';
import type { PluginSDK } from '../../../packages/plugin-sdk/src';
import { VOUCH_COLLECTIONS } from '../service';
import { buildProofPayload, buildVouchPayload, type ThresholdProof, type Vouch, type VouchRequest } from '../model';

// ---------------------------------------------------------------------------
// mock 数据：一个担保请求（被担保人 rootId 为长字符串自报文本）+ 两份担保
// （门槛 2 已满足）+ 一个已组装证明（载荷按 model 纯函数构造，验签可全过）
// ---------------------------------------------------------------------------

const NOW_MS = 1_700_000_000_000;
const REQUEST_ID = 'req_1';
const PROOF_ID = 'proof_1';
const CONTEXT = 'affair_' + 'ab'.repeat(16);
const SUBJECT_ROOT = 'root-subject-' + 'cd'.repeat(24);
const VOUCHER_1 = 'root-voucher-' + 'ef'.repeat(24);
const VOUCHER_2 = 'root-voucher-' + 'ab'.repeat(24);

const REQUEST: VouchRequest = {
  requestId: REQUEST_ID,
  context: CONTEXT,
  subjectRootId: SUBJECT_ROOT,
  requiredCount: 2,
  note: '加入项目空间需要两名老成员担保',
  createdAt: NOW_MS - 3_600_000
};

const VOUCHES: Vouch[] = [VOUCHER_1, VOUCHER_2].map((voucherRootId, index) => ({
  requestId: REQUEST_ID,
  voucherRootId,
  payload: buildVouchPayload(REQUEST_ID, CONTEXT, SUBJECT_ROOT, voucherRootId),
  signature: `sig-${index}`,
  publicKey: `pk-${index}`,
  vouchedAt: NOW_MS - 1_800_000 + index
}));

const PROOF: ThresholdProof = {
  proofId: PROOF_ID,
  requestId: REQUEST_ID,
  context: CONTEXT,
  subjectRootId: SUBJECT_ROOT,
  requiredCount: 2,
  vouches: VOUCHES,
  assembledBy: VOUCHER_1,
  assembledAt: NOW_MS - 600_000,
  payload: buildProofPayload(PROOF_ID, REQUEST_ID, CONTEXT, SUBJECT_ROOT, VOUCHES),
  signature: 'sig-proof',
  publicKey: 'pk-proof'
};

// ---------------------------------------------------------------------------
// mock SDK：docs 三集合查询 + identity 签名/验签 + runtime.currentRoot
// ---------------------------------------------------------------------------

function createMockSdk() {
  return {
    runtime: {
      currentRoot: vi.fn().mockResolvedValue({ rootId: VOUCHER_1, unlocked: true })
    },
    docs: {
      defineCollection: vi.fn().mockResolvedValue({}),
      put: vi.fn().mockResolvedValue({}),
      query: vi.fn((collection: string) => {
        const items =
          collection === VOUCH_COLLECTIONS.requests ? [REQUEST]
          : collection === VOUCH_COLLECTIONS.vouches ? VOUCHES
          : collection === VOUCH_COLLECTIONS.proofs ? [PROOF]
          : [];
        return Promise.resolve({ items: items.map((data) => ({ data })) });
      })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({ signature: 'sig', publicKey: 'pk', payloadHash: 'ph' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    }
  } as unknown as PluginSDK;
}

// ---------------------------------------------------------------------------
// 挂载辅助
// ---------------------------------------------------------------------------

let mounted: Array<{ app: App; host: HTMLElement }> = [];
let VouchView: Component;

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 等 onMounted 异步链（SDK 注入 → 请求/证明列表加载 → 门槛计数刷新）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mount(): Promise<HTMLElement> {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(VouchView);
  app.use(ElementPlus);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

beforeAll(async () => {
  // SDK 注入后再挂载（ensurePluginSDK 轮询 window.__sparkPluginSDK）
  (window as unknown as { __sparkPluginSDK: PluginSDK }).__sparkPluginSDK = createMockSdk();
  VouchView = (await import('../VouchView.vue')).default;
});

afterEach(() => {
  for (const { app, host } of mounted) {
    app.unmount();
    host.remove();
  }
  mounted = [];
});

describe('spark-threshold-vouch 窗口化布局三档断言', () => {
  it.each([320, 480, 880])('窗口宽 %i：头卡/发起表单/请求条目/证明条目结构完整，无壳层重复元素', async (width) => {
    setViewport(width);
    const host = await mount();

    // 头卡：标题 + 刷新钮 + 诚实口径警示
    expect(host.querySelector('.header-card h2')?.textContent).toBe('担保链门槛');
    expect(host.textContent ?? '').toContain('演示级实现');

    // 发起表单：上下文/被担保人/门槛/说明四字段 + 发起钮
    expect(host.querySelector('.composer-card')).not.toBeNull();
    expect(host.querySelectorAll('.composer-card .el-form-item')).toHaveLength(4);
    expect(host.textContent ?? '').toContain('发起请求');

    // 请求条目：上下文 + 担保进度 tag + 长 rootId 自报文本全文渲染（折行为 CSS 职责）
    expect(host.querySelector('.request-meta strong')?.textContent).toBe(CONTEXT);
    expect(host.querySelector('.request-meta .el-tag')?.textContent).toContain('2/2 份担保');
    const requestItem = host.querySelector('.request-item')!;
    const requestHint = requestItem.querySelector('.hint')?.textContent ?? '';
    expect(requestHint).toContain(SUBJECT_ROOT);
    // 操作行：担保输入框 + 签名担保 + 组装门槛证明（门槛已满足，组装钮可用）
    expect(requestItem.querySelector('.voucher-input input')).not.toBeNull();
    const actionTexts = [...requestItem.querySelectorAll<HTMLButtonElement>('.actions .el-button')].map((b) =>
      b.textContent?.trim()
    );
    expect(actionTexts).toEqual(['签名担保（插件域身份）', '组装门槛证明']);

    // 证明条目：担保份数 tag + 验证状态 + 长组装人 id 自报文本 + 验签钮
    expect(host.textContent ?? '').toContain('2 份担保 / 门槛 2');
    expect(host.textContent ?? '').toContain('未验证');
    expect(host.textContent ?? '').toContain(`组装人（自报）${VOUCHER_1}`);
    expect(host.textContent ?? '').toContain('验签（免权限，验插件域签名）');

    // 重复壳检查：无「关闭应用/返回桌面/退出」类壳元素
    expect(host.textContent ?? '').not.toMatch(/关闭应用|返回桌面|退出/);
  });

  it.each([320, 480, 880])('窗口宽 %i：验签展开逐项检查结论，长担保人 id 检查项完整渲染', async (width) => {
    setViewport(width);
    const host = await mount();

    const verifyButton = [...host.querySelectorAll<HTMLButtonElement>('.el-button')].find((b) =>
      b.textContent?.includes('验签（免权限')
    );
    expect(verifyButton).not.toBeUndefined();
    verifyButton!.click();
    await flushMount();

    // 状态 tag 翻转 + 逐项检查：载荷完整性/组装人签名/无重复/两份担保签名/门槛满足
    expect(host.textContent ?? '').toContain('验证通过');
    const checks = host.querySelectorAll('.checks li');
    expect(checks).toHaveLength(3 + VOUCHES.length + 1);
    const checkTexts = [...checks].map((li) => li.textContent ?? '');
    expect(checkTexts.some((t) => t.includes('证明载荷完整性'))).toBe(true);
    expect(checkTexts.some((t) => t.includes('组装人签名'))).toBe(true);
    expect(checkTexts.some((t) => t.includes('担保人无重复'))).toBe(true);
    expect(checkTexts.some((t) => t.includes(`担保签名 ${VOUCHER_1}`))).toBe(true);
    expect(checkTexts.some((t) => t.includes('门槛满足'))).toBe(true);
  });
});

describe('spark-threshold-vouch 滚动契约与窄窗适配静态断言', () => {
  const source = readFileSync(resolve(process.cwd(), '../plugins/spark-threshold-vouch/VouchView.vue'), 'utf8');

  it('主视图根节点不设 height/overflow，全文无 100vh（超高列表依赖 iframe 原生文档滚动）', () => {
    // import.meta.url 在 vitest 下带 /@fs/ 前缀不可直接读，从工作目录（code/app）解析
    expect(source).not.toMatch(/100vh/);
    // 根节点不得成为裁切容器（文本截断省略的局部 overflow:hidden 不在此列）
    const rootRule = source.match(/\.spark-threshold-vouch\s*\{([^}]*)\}/);
    expect(rootRule?.[1] ?? '').not.toMatch(/overflow|height/);
  });

  it('窄窗适配规则存在：操作行 flex-wrap 换行、长 rootId/context/检查项任意断行', () => {
    // 担保输入框 + 签名担保 + 组装证明三件套在 320 窗口一行放不下，必须允许换行
    const actionsRule = source.match(/\.actions\s*\{([^}]*)\}/);
    expect(actionsRule?.[1] ?? '').toMatch(/flex-wrap:\s*wrap/);
    // 输入框弹性收缩（否则 flex 项按内容最小宽撑开，窄窗仍溢出）
    const inputRule = source.match(/\.voucher-input\s*\{([^}]*)\}/);
    expect(inputRule?.[1] ?? '').toMatch(/min-width:\s*0/);
    // 长 rootId（自报文本，无空格）/ 长 context / 检查项名任意断行
    for (const pattern of [/\.hint\s*\{([^}]*)\}/, /\.request-meta strong\s*\{([^}]*)\}/, /\.checks li\s*\{([^}]*)\}/]) {
      const rule = source.match(pattern);
      expect(rule?.[1] ?? '').toMatch(/overflow-wrap:\s*anywhere/);
    }
  });

  it('manifest 声明 window 默认尺寸 640×560（单列工具型插件，窄于壳层缺省 880×620）且在壳层合法范围内', () => {
    const manifest = JSON.parse(readFileSync(resolve(process.cwd(), '../plugins/spark-threshold-vouch/manifest.json'), 'utf8')) as {
      window?: { defaultWidth?: number; defaultHeight?: number };
    };
    // 与壳层 normalize_window 合法范围同口径（宽 320–3840 / 高 220–2160）
    expect(manifest.window).toEqual({ defaultWidth: 640, defaultHeight: 560 });
  });
});
