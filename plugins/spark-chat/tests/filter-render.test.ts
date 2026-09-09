/**
 * 个人过滤规则渲染前过滤的组件测试（communication §4.3）：
 * 挂载真实 ChatView（ElementPlus 全量注册，与插件入口 index.ts 同口径），
 * store 注入消息后断言命中规则的消息不进入渲染输出——
 * 关键词命中 / 来源命中 / 组合规则（或语义 + 禁用恢复）/ 自己的消息不过滤。
 * 纯内存模式（未绑定 SDK）：过滤规则与消息都只在内存，验证渲染通路的纯本地性。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApp, h, nextTick, type App } from 'vue';
import ElementPlus from 'element-plus';
import type { PluginContext, PluginSDK } from '../../../packages/plugin-sdk/src';
import ChatView from '../src/components/ChatView.vue';
import { bindPluginRuntime } from '../src/sdk-host';
import { onChatReceived, resetMessagesCache, type ChatMessage, type Conversation } from '../src/store';
import {
  addKeywordRule,
  addSourceRule,
  resetFilterRulesCache,
  setRuleEnabled
} from '../src/filter-rules';

let fixtureSeq = 0;
let mounted: Array<{ app: App; host: HTMLElement }> = [];

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

/** 注入一条对端消息（会话随首条消息建立），返回会话与消息 */
function pushMessage(spaceKey: string, peerId: string, content: string, senderId = peerId): { conv: Conversation; msg: ChatMessage } {
  fixtureSeq += 1;
  const conv: Conversation = {
    id: `dm:${peerId}`,
    kind: 'direct',
    title: `对方-${peerId}`,
    peerId,
    unreadCount: 0,
    pinnedAt: 0,
    muted: false,
    online: false,
    draft: '',
    updatedAt: 1_000 + fixtureSeq
  };
  const msg: ChatMessage = {
    id: `m-${fixtureSeq}`,
    senderId,
    senderName: senderId === 'me' ? '我' : conv.title,
    type: 'text',
    content,
    createdAt: conv.updatedAt,
    recalled: false,
    ...(senderId === 'me' ? { status: 'delivered' as const } : {})
  };
  onChatReceived({ spaceKey, conversation: conv, message: msg });
  return { conv, msg };
}

/** 挂载 ChatView 并等首帧渲染完成，返回宿主元素 */
async function mountChatView(spaceKey: string, conversationId: string): Promise<HTMLElement> {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(ChatView, { spaceKey, conversationId }) });
  app.use(ElementPlus);
  app.mount(host);
  mounted.push({ app, host });
  await nextTick();
  return host;
}

/** 当前渲染出的消息文本（.msg-text 为文本消息的内容节点） */
function renderedTexts(host: HTMLElement): string[] {
  return Array.from(host.querySelectorAll('.msg-text')).map((el) => (el.textContent ?? '').trim());
}

beforeEach(() => {
  // 未绑定 SDK：纯内存模式（store 与过滤规则都不发任何数据面调用）
  bindPluginRuntime(null as unknown as PluginSDK, fakeCtx());
  resetMessagesCache();
  resetFilterRulesCache();
});

afterEach(() => {
  for (const { app, host } of mounted) {
    app.unmount();
    host.remove();
  }
  mounted = [];
});

describe('个人过滤规则：渲染前过滤（ChatView 组件）', () => {
  it('无规则时全部消息正常渲染（过滤不改变默认行为）', async () => {
    const key = 'personal';
    pushMessage(key, 'peer-a', '正常消息一');
    pushMessage(key, 'peer-a', '正常消息二');
    const host = await mountChatView(key, 'dm:peer-a');
    expect(renderedTexts(host)).toEqual(['正常消息一', '正常消息二']);
  });

  it('关键词命中：含关键词的消息不渲染，其余照渲染', async () => {
    const key = 'personal';
    pushMessage(key, 'peer-b', '今晚一起吃饭吗');
    pushMessage(key, 'peer-b', 'ADS 大促销，点击立减');
    pushMessage(key, 'peer-b', '周末有空吗');
    const host = await mountChatView(key, 'dm:peer-b');
    expect(renderedTexts(host)).toHaveLength(3);

    addKeywordRule('ads'); // 大小写不敏感
    await nextTick();
    expect(renderedTexts(host)).toEqual(['今晚一起吃饭吗', '周末有空吗']);
  });

  it('来源命中：被屏蔽 senderId 的消息一律不渲染', async () => {
    const key = 'personal';
    pushMessage(key, 'peer-c1', '来自甲的消息');
    // 同会话内的其他来源（bot/转发形态）：senderId 不同于会话 peer
    pushMessage(key, 'peer-c1', '来自乙的消息', 'peer-c2');
    pushMessage(key, 'peer-c1', '来自甲的第二条');
    const host = await mountChatView(key, 'dm:peer-c1');
    expect(renderedTexts(host)).toHaveLength(3);

    addSourceRule('peer-c2');
    await nextTick();
    expect(renderedTexts(host)).toEqual(['来自甲的消息', '来自甲的第二条']);
  });

  it('组合规则：关键词 + 来源或语义同时生效，禁用其一即恢复对应消息', async () => {
    const key = 'personal';
    pushMessage(key, 'peer-d', '普通问候');
    pushMessage(key, 'peer-d', '这是广告内容');
    pushMessage(key, 'peer-d', '来自屏蔽源的消息', 'peer-d2');
    const host = await mountChatView(key, 'dm:peer-d');
    expect(renderedTexts(host)).toHaveLength(3);

    const kw = addKeywordRule('广告')!;
    addSourceRule('peer-d2');
    await nextTick();
    expect(renderedTexts(host)).toEqual(['普通问候']);

    setRuleEnabled(kw.id, false);
    await nextTick();
    expect(renderedTexts(host)).toEqual(['普通问候', '这是广告内容']);
  });

  it('自己发出的消息不受关键词过滤（用户应看得见自己刚发出的内容）', async () => {
    const key = 'personal';
    pushMessage(key, 'peer-e', '对方的消息');
    pushMessage(key, 'peer-e', '我也提到广告这个词', 'me');
    const host = await mountChatView(key, 'dm:peer-e');
    addKeywordRule('广告');
    await nextTick();
    expect(renderedTexts(host)).toContain('我也提到广告这个词');
    expect(renderedTexts(host)).toContain('对方的消息');
  });
});
