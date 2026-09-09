/**
 * 窗口化适配断言（PC 插件窗口化，ui-architecture §4.2；窗口最小夹取 320×220）。
 *
 * 主视图为单列卡片流（el-card 纵向 grid、无宽度断点），320/480/880 三档同构——
 * 断言聚焦结构不变量而非像素（jsdom 不做排版）：
 * 1. 三档视口下头卡/组织选择/发帖框/时间线结构完整，长 RootID 走截断省略结构
 *    （root-id-text / post-meta strong），无壳层重复元素；
 * 2. 帖子与评论树（含嵌套回复）渲染完整，评论区可展开收起；
 * 3. 重复壳检查：本插件非沉浸式（manifest 未声明 chrome.hostTitleBar:false，
 *    移动全屏由壳层顶栏提供「< 返回」），视图内不得出现「关闭应用/返回桌面/退出」
 *    类元素，因此也不需要 moments 那样的 (pointer:coarse) 退出钮显隐逻辑；
 * 4. 滚动契约：宿主 srcdoc 的 body 无 overflow 限制，主视图根节点不设
 *    height/overflow，超高内容依赖 iframe 原生文档滚动——moments 修过的
 *    「overflow:hidden 父级裁切超高内容」bug 类在本插件不适用；用源码静态
 *    断言锁住这个前提（根节点不设 height/overflow、全文无 100vh；文本截断
 *    省略的局部 overflow:hidden 不在此列）。
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick, type App, type Component } from 'vue';
import ElementPlus from 'element-plus';
import type { PluginSDK } from '../../../packages/plugin-sdk/src';
import { WEIBO_COLLECTIONS } from '../service';

// ---------------------------------------------------------------------------
// mock 数据：一个组织（当前身份为管理员）、两条帖（其一已签名）、一条评论 + 一条回复
// ---------------------------------------------------------------------------

const ROOT_ID = 'root-admin-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const ORG_ID = 'org-1';

const ORG = {
  orgId: ORG_ID,
  name: '示例组织',
  description: '',
  members: [{ rootId: ROOT_ID, role: 'admin', nodeInfo: { addresses: [] } }]
};

const ORG_CONFIG = { orgId: ORG_ID, superAdminRootId: ROOT_ID, createdBy: ROOT_ID, createdAt: 1 };

const POSTS = [
  {
    id: 'post-2',
    orgId: ORG_ID,
    content: '第二条短文',
    authorRootId: ROOT_ID,
    createdAt: 2000,
    signature: { payload: 'p', signature: 's', publicKey: 'pk' }
  },
  { id: 'post-1', orgId: ORG_ID, content: '第一条短文', authorRootId: ROOT_ID, createdAt: 1000 }
];

const COMMENTS = [
  { id: 'comment-1', orgId: ORG_ID, postId: 'post-1', content: '一条评论', authorRootId: 'root-member', createdAt: 1100 },
  {
    id: 'reply-1',
    orgId: ORG_ID,
    postId: 'post-1',
    parentCommentId: 'comment-1',
    content: '一条回复',
    authorRootId: 'root-member-2',
    createdAt: 1200
  }
];

// ---------------------------------------------------------------------------
// mock SDK：覆盖主视图挂载链路（runtime / docs / messages.onCardAction）
// ---------------------------------------------------------------------------

function createMockSdk() {
  return {
    runtime: {
      currentRoot: vi.fn().mockResolvedValue({ rootId: ROOT_ID, unlocked: true }),
      listMineOrganizations: vi.fn().mockResolvedValue([ORG]),
      syncOrganizationData: vi.fn().mockResolvedValue(undefined)
    },
    docs: {
      defineCollection: vi.fn().mockResolvedValue({}),
      get: vi.fn().mockResolvedValue(ORG_CONFIG),
      put: vi.fn().mockResolvedValue({}),
      query: vi.fn((collection: string) => {
        const items = collection === WEIBO_COLLECTIONS.posts ? POSTS : collection === WEIBO_COLLECTIONS.comments ? COMMENTS : [];
        return Promise.resolve({ items: items.map((data) => ({ data })) });
      })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({ signature: 'sig', publicKey: 'pk', payloadHash: 'ph' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    messages: {
      sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }),
      onCardAction: vi.fn(() => () => {})
    }
  } as unknown as PluginSDK;
}

// ---------------------------------------------------------------------------
// 挂载辅助
// ---------------------------------------------------------------------------

let mounted: Array<{ app: App; host: HTMLElement }> = [];
let ExampleView: Component;

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 等 onMounted 异步链（SDK 注入 → 组织/时间线加载）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mount(): Promise<HTMLElement> {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(ExampleView);
  app.use(ElementPlus);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

beforeAll(async () => {
  // SDK 注入后再挂载（ensurePluginSDK 轮询 window.__sparkPluginSDK）
  (window as unknown as { __sparkPluginSDK: PluginSDK }).__sparkPluginSDK = createMockSdk();
  ExampleView = (await import('../ExampleView.vue')).default;
});

afterEach(() => {
  for (const { app, host } of mounted) {
    app.unmount();
    host.remove();
  }
  mounted = [];
});

describe('spark-example 窗口化布局三档断言', () => {
  it.each([320, 480, 880])('窗口宽 %i：头卡/组织选择/发帖框/时间线结构完整，无壳层重复元素', async (width) => {
    setViewport(width);
    const host = await mount();

    // 头卡：标题 + 刷新 + 组织选择 + RootID tag（长 ID 走截断省略结构）
    expect(host.querySelector('.header-card h2')?.textContent).toBe('组织微博');
    expect(host.querySelector('.root-id-tag .root-id-text')?.textContent).toContain(ROOT_ID);
    expect(host.querySelector('.selectors .el-select')).not.toBeNull();

    // 发帖框：textarea + 发送钮 + 字数上限
    expect(host.querySelector('.composer-card textarea')).not.toBeNull();
    expect(host.querySelector('.composer-card .counter')?.textContent).toBe('0/260');
    const postButton = [...host.querySelectorAll<HTMLButtonElement>('.composer-card .actions .el-button')].find((b) =>
      b.textContent?.includes('发送短文')
    );
    expect(postButton).not.toBeUndefined();
    expect(postButton?.disabled).toBe(false); // 当前身份是组织管理员

    // 时间线：两条帖全部渲染（超高内容由 iframe 原生滚动承载，见文件头滚动契约）
    const items = host.querySelectorAll('.post-item');
    expect(items).toHaveLength(2);
    expect(host.querySelector('.post-meta strong')?.textContent).toBe(ROOT_ID);
    expect(host.textContent ?? '').toContain('已签名');

    // 重复壳检查：无「关闭应用/返回桌面/退出」类壳元素
    expect(host.textContent ?? '').not.toMatch(/关闭应用|返回桌面|退出/);
  });

  it.each([320, 480, 880])('窗口宽 %i：评论区可展开，评论树（含嵌套回复）与回复编辑器完整', async (width) => {
    setViewport(width);
    const host = await mount();

    const toggle = [...host.querySelectorAll<HTMLButtonElement>('.post-item .comment-toggle .el-button')].find((b) =>
      b.textContent?.includes('评论（2）')
    );
    expect(toggle).not.toBeUndefined();
    toggle!.click();
    await flushMount();

    const firstPost = host.querySelector('#post-post-1')!;
    expect(firstPost.querySelectorAll('.comment-item')).toHaveLength(2);
    expect(firstPost.querySelectorAll('.comment-item.nested')).toHaveLength(1);
    // 评论编辑器 + 每条根评论的回复编辑器（flex 行：输入框收缩、按钮不收缩）
    expect(firstPost.querySelectorAll('.reply-editor')).toHaveLength(2);
    expect(firstPost.textContent ?? '').toContain('回复：一条回复');
  });
});

describe('spark-example 滚动契约静态断言', () => {
  it('主视图根节点不设 height/overflow，全文无 100vh（超高内容依赖 iframe 原生文档滚动）', () => {
    // import.meta.url 在 vitest 下带 /@fs/ 前缀不可直接读，从工作目录（code/app）解析
    const source = readFileSync(resolve(process.cwd(), '../plugins/spark-example/ExampleView.vue'), 'utf8');
    expect(source).not.toMatch(/100vh/);
    // 根节点不得成为裁切容器（文本省略的 overflow:hidden 是局部截断，不在此列）
    const rootRule = source.match(/\.spark-example\s*\{([^}]*)\}/);
    expect(rootRule?.[1] ?? '').not.toMatch(/overflow|height/);
  });
});
