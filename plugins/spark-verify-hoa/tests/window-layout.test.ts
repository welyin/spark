/**
 * 窗口化适配断言（PC 插件窗口化，ui-architecture §4.2；窗口最小夹取 320×220）。
 *
 * 申请人/验证人/凭证三侧经 el-tabs 分页（非双栏布局，窄窗天然单列堆叠，
 * tabs 头溢出由 Element Plus 自带滚动箭头承载）——断言聚焦结构不变量而非
 * 像素（jsdom 不做排版）：
 * 1. 三档视口（320/480/880）下头卡/组织选择器/三 tabs/申请人表单（验证方式 +
 *    凭证类型 + 户号 + 材料行）/验证人审核条目（含长自报 rootId 全文、材料
 *    哈希行、签发钮）/已签发凭证条目（注销输入框 + 注销钮）/凭证自查与内核
 *    凭证面（内核验证/出示/验证人信任声明/注销快照查询）结构完整，无壳层重复元素；
 * 2. 交互面：演示自查回显演示级自查结论，内核验证回显结构化裁决；
 * 3. 重复壳检查：本插件非沉浸式（manifest 未声明 chrome.hostTitleBar:false，
 *    移动全屏由壳层顶栏提供返回），视图内不得出现「关闭应用/返回桌面/退出」
 *    类壳元素；
 * 4. 滚动契约：主视图根节点不设 height/overflow，全文无 100vh——超高申请/
 *    凭证列表由 iframe 原生文档滚动承载（源码静态断言，moments 修过的
 *    「overflow:hidden 父级裁切超高内容」bug 类在本插件不适用）；
 * 5. 窄窗适配样式静态锁定：操作行/材料行 flex-wrap（320 档材料名 + 内容 +
 *    移除钮、注销输入框 + 钮单行必然溢出，必须允许换行）、输入框弹性收缩
 *    （min-width:0）、长哈希/自报 rootId 任意断行（overflow-wrap）——jsdom
 *    不排版，锁规则存在；
 * 6. manifest 声明 window 默认尺寸 720×600（表单工具型：三个分页均含表单 +
 *    列表，宽于 threshold-vouch 的 640×560、窄于壳层缺省 880×620），且在
 *    壳层合法范围内（宽 320–3840 / 高 220–2160）。
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick, type App, type Component } from 'vue';
import ElementPlus from 'element-plus';
import type { PluginSDK } from '../../../packages/plugin-sdk/src';
import { HOA_COLLECTIONS } from '../service';
import { buildCredentialSignPayload, type HoaCredential, type VerificationApplication } from '../model';

// ---------------------------------------------------------------------------
// mock 数据：一个小区组织 + 一条待审核申请（申请人 rootId 为无空格长自报文本，
// 材料哈希 64 位 hex）+ 一条历史申请下已签发的演示凭证（签发人 rootId 同为长
// 自报文本；applicationId 与待审核申请错开，保持「待审核」状态）+ 一枚本机
// 持有的协议凭证（内核凭证面渲染）
// ---------------------------------------------------------------------------

const NOW_MS = 1_700_000_000_000;
const ORG_ID = `org_${'cd'.repeat(8)}`;
const APPLICANT_ROOT = 'root-applicant-' + 'ab'.repeat(24);
const ISSUER_ROOT = 'root-issuer-' + 'cd'.repeat(24);
const MATERIAL_HASH = 'ef'.repeat(32);
const HELD_CRED_ID = 'ab'.repeat(32);
const HELD_ISSUER = '10'.repeat(32);

const APPLICATION: VerificationApplication = {
  applicationId: 'app-1',
  orgId: ORG_ID,
  applicantRootId: APPLICANT_ROOT,
  method: 'deed-manual',
  credentialType: 'owner',
  unitNo: '3-502',
  materials: [{ label: '房产证照片', contentHash: MATERIAL_HASH, byteSize: 2048 }],
  createdAt: NOW_MS - 3_600_000
};

const CREDENTIAL: HoaCredential = {
  credentialId: 'cred-1',
  applicationId: 'app-0',
  orgId: ORG_ID,
  subjectRootId: APPLICANT_ROOT,
  credentialType: 'resident',
  unitNo: '5-301',
  method: 'vouch-2',
  issuerRootId: ISSUER_ROOT,
  issuedAt: NOW_MS - 1_800_000,
  signature: {
    payload: buildCredentialSignPayload(ORG_ID, 'cred-1', APPLICANT_ROOT, 'resident', '5-301', 'vouch-2'),
    signature: 'sig-1',
    publicKey: 'pk-1'
  }
};

const HELD = {
  credId: HELD_CRED_ID,
  credential: { credType: 'owner', method: 'deed-manual', issuer: { identity: HELD_ISSUER } }
};

// ---------------------------------------------------------------------------
// mock SDK：runtime 组织列表 + docs 三集合查询 + identity 签名/验签 +
// credentials 只读五方法（按已落地 SDK 面钉住契约，见 sdk-credentials.ts）
// ---------------------------------------------------------------------------

function createMockSdk() {
  return {
    runtime: {
      currentRoot: vi.fn().mockResolvedValue({ rootId: ISSUER_ROOT, unlocked: true }),
      listMineOrganizations: vi.fn().mockResolvedValue([{ orgId: ORG_ID, name: '阳光花园' }])
    },
    docs: {
      defineCollection: vi.fn().mockResolvedValue({}),
      put: vi.fn().mockResolvedValue({ success: true }),
      query: vi.fn((collection: string) => {
        const items =
          collection === HOA_COLLECTIONS.applications ? [APPLICATION]
          : collection === HOA_COLLECTIONS.credentials ? [CREDENTIAL]
          : [];
        return Promise.resolve({ items: items.map((data) => ({ data })) });
      })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({ signature: 'sig', publicKey: 'pk', payloadHash: 'ph' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    credentials: {
      listHeld: vi.fn().mockResolvedValue([HELD]),
      presentHolderProof: vi.fn().mockResolvedValue({
        credential: { credV: 1 },
        holderProof: { credId: HELD_CRED_ID, sig: 'proof-sig' },
        presentedAt: NOW_MS
      }),
      queryVerifiers: vi.fn().mockResolvedValue({ orgId: ORG_ID, effectiveFrom: 0, seq: 1, updatedAt: 0, verifiers: [] }),
      verify: vi.fn().mockResolvedValue({
        credId: HELD_CRED_ID,
        valid: true,
        checks: { static: true, trust: true, revocation: 'not-revoked' },
        reason: null
      }),
      queryRevocations: vi.fn().mockResolvedValue({ issuer: HELD_ISSUER, available: false })
    }
  } as unknown as PluginSDK;
}

// ---------------------------------------------------------------------------
// 挂载辅助
// ---------------------------------------------------------------------------

let mounted: Array<{ app: App; host: HTMLElement }> = [];
let HoaVerifyView: Component;

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 等 onMounted 异步链（SDK 注入 → 组织列表 → 申请/凭证/注销/持有凭证加载）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mount(): Promise<HTMLElement> {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(HoaVerifyView);
  app.use(ElementPlus);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

function buttonTexts(host: HTMLElement): string[] {
  return [...host.querySelectorAll<HTMLButtonElement>('.el-button')].map((b) => b.textContent?.trim() ?? '');
}

beforeAll(async () => {
  // SDK 注入后再挂载（ensurePluginSDK 轮询 window.__sparkPluginSDK）
  (window as unknown as { __sparkPluginSDK: PluginSDK }).__sparkPluginSDK = createMockSdk();
  HoaVerifyView = (await import('../HoaVerifyView.vue')).default;
});

afterEach(() => {
  for (const { app, host } of mounted) {
    app.unmount();
    host.remove();
  }
  mounted = [];
});

describe('spark-verify-hoa 窗口化布局三档断言', () => {
  it.each([320, 480, 880])('窗口宽 %i：头卡/三 tabs/双侧条目/内核凭证面结构完整，无壳层重复元素', async (width) => {
    setViewport(width);
    const host = await mount();
    const text = host.textContent ?? '';
    const buttons = buttonTexts(host);

    // 头卡：标题 + 诚实口径 lede + 刷新钮 + 组织选择器
    expect(host.querySelector('.header-card h2')?.textContent).toBe('业主资格验证');
    expect(text).toContain('凭证只暴露资格结论（户号）');
    expect(buttons).toContain('刷新');
    expect(host.querySelector('.header-card .el-select')).not.toBeNull();

    // 三 tabs：申请人/验证人/凭证（非双栏，窄窗天然单列；非懒加载全部在 DOM）
    const tabTexts = [...host.querySelectorAll('.el-tabs__item')].map((tab) => tab.textContent?.trim());
    expect(tabTexts).toEqual(['申请人：提交材料', '验证人：审核签发', '凭证：自查 / 内核凭证面']);

    // 申请人侧表单：验证方式 + 凭证类型 + 户号 + 材料行（名称 + 内容 + 移除）+ 提交
    expect(text).toContain('验证方式（组织加入规则声明接受的组合）');
    expect(host.querySelectorAll('.el-radio-button')).toHaveLength(2);
    expect(host.querySelector('input[placeholder="如 3-502"]')).not.toBeNull();
    const materialRow = host.querySelector('.material-row')!;
    expect(materialRow.querySelector('.material-label input')).not.toBeNull();
    expect(materialRow.querySelector('textarea')).not.toBeNull();
    expect(buttons).toContain('移除');
    expect(buttons).toContain('+ 添加材料');
    expect(buttons).toContain('提交申请');

    // 验证人侧审核条目：户号 + 业主/方式/状态 tags + 长自报 rootId 全文 + 材料哈希行 + 签发钮
    expect(host.querySelector('.record-item strong')?.textContent).toBe('3-502');
    expect(text).toContain('待审核');
    expect(text).toContain('房产证人工核验');
    expect(text).toContain(`申请人（自报）${APPLICANT_ROOT}`);
    expect(text).toContain(`哈希 ${MATERIAL_HASH}`);
    expect(buttons).toContain('签发演示凭证');

    // 已签发凭证条目：户号 + 长签发人自报文本 + 注销输入框 + 注销钮
    expect(text).toContain('5-301');
    expect(text).toContain(`签发人（自报）${ISSUER_ROOT}`);
    expect(host.querySelector('.revoke-input input')?.getAttribute('placeholder')).toBe('注销理由（资格变更）');
    expect(buttons).toContain('注销');

    // 凭证页：演示自查 + 内核凭证面（持有凭证 + 内核验证/查注销快照 + 出示表单 + 信任声明/注销快照查询）
    expect(buttons).toContain('演示自查');
    expect(text).toContain('内核凭证面（sdk.credentials 只读）');
    expect(text).toContain('deed-manual');
    expect(buttons).toContain('内核验证');
    expect(buttons).toContain('查签发人注销快照');
    expect(host.querySelector('input[placeholder="集合（如 members）"]')).not.toBeNull();
    expect(buttons).toContain('出示');
    expect(buttons).toContain('查询本组织验证人信任声明');
    expect(host.querySelector('input[placeholder="签发人身份 id（64 位小写 hex）"]')).not.toBeNull();

    // 重复壳检查：无「关闭应用/返回桌面/退出」类壳元素
    expect(text).not.toMatch(/关闭应用|返回桌面|退出/);
  });

  it.each([320, 480, 880])('窗口宽 %i：演示自查与内核验证如实回显结论', async (width) => {
    setViewport(width);
    const host = await mount();

    // 演示自查：重算载荷 + identity.verify + 本地注销表（mock 全过）→ 演示级口径回显
    const demoButton = [...host.querySelectorAll<HTMLButtonElement>('.el-button')].find((b) =>
      b.textContent?.includes('演示自查')
    );
    demoButton!.click();
    await flushMount();
    expect(host.textContent ?? '').toContain('自查结果：通过（演示级自查');

    // 内核验证链：结构化裁决逐项回显（静态链/信任链/注销检查）
    const kernelButton = [...host.querySelectorAll<HTMLButtonElement>('.el-button')].find((b) =>
      b.textContent?.includes('内核验证')
    );
    kernelButton!.click();
    await flushMount();
    expect(host.textContent ?? '').toContain('内核裁决：有效（静态链 过 · 信任链 匹配 · 注销检查 not-revoked）');
  });
});

describe('spark-verify-hoa 滚动契约与窄窗适配静态断言', () => {
  const source = readFileSync(resolve(process.cwd(), '../plugins/spark-verify-hoa/HoaVerifyView.vue'), 'utf8');

  it('主视图根节点不设 height/overflow，全文无 100vh（超高列表依赖 iframe 原生文档滚动）', () => {
    // import.meta.url 在 vitest 下带 /@fs/ 前缀不可直接读，从工作目录（code/app）解析
    expect(source).not.toMatch(/100vh/);
    // 根节点不得成为裁切容器（文本截断省略的局部 overflow:hidden 不在此列）
    const rootRule = source.match(/\.spark-verify-hoa\s*\{([^}]*)\}/);
    expect(rootRule?.[1] ?? '').not.toMatch(/overflow|height/);
  });

  it('窄窗适配规则存在：操作行/材料行 flex-wrap 换行、输入框弹性收缩、长哈希/自报 rootId 任意断行', () => {
    // 注销输入框 + 注销钮、材料名 + 内容 + 移除钮在 320 窗口一行放不下，必须允许换行
    for (const pattern of [/\.actions\s*\{([^}]*)\}/, /\.material-row\s*\{([^}]*)\}/]) {
      const rule = source.match(pattern);
      expect(rule?.[1] ?? '').toMatch(/flex-wrap:\s*wrap/);
    }
    // 输入框弹性收缩（否则 flex 项按内容最小宽撑开，窄窗仍溢出）
    for (const pattern of [/\.material-label\s*\{([^}]*)\}/, /\.material-row \.el-textarea\s*\{([^}]*)\}/, /\.revoke-input\s*\{([^}]*)\}/]) {
      const rule = source.match(pattern);
      expect(rule?.[1] ?? '').toMatch(/min-width:\s*0/);
    }
    // 材料哈希（64 位 hex）/自报 rootId（无空格长字符串）任意断行
    for (const pattern of [/\.hint\s*\{([^}]*)\}/, /\.record-origin\s*\{([^}]*)\}/]) {
      const rule = source.match(pattern);
      expect(rule?.[1] ?? '').toMatch(/overflow-wrap:\s*anywhere/);
    }
  });

  it('manifest 声明 window 默认尺寸 720×600（表单工具型，窄于壳层缺省 880×620）且在壳层合法范围内', () => {
    const manifest = JSON.parse(readFileSync(resolve(process.cwd(), '../plugins/spark-verify-hoa/manifest.json'), 'utf8')) as {
      window?: { defaultWidth?: number; defaultHeight?: number };
    };
    // 与壳层 normalize_window 合法范围同口径（宽 320–3840 / 高 220–2160）
    expect(manifest.window).toEqual({ defaultWidth: 720, defaultHeight: 600 });
  });
});
