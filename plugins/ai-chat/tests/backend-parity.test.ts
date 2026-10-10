/**
 * 双端解析器一致性锚定（评审 U2）：background.ts 因 QuickJS 零依赖约束内联了
 * 与 service.ts 同口径的 SSE / Ollama 流式解析器，两侧无法共享代码。本测试把
 * 同一份线形数据分别喂给两侧实现（UI 侧直接调 service 解析器；后台侧经假宿主
 * fetchStream 驱动真实 background 脚本），断言逐 token 序列一致——任一侧改
 * 口径都会在这里爆炸。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createOllamaStreamParser, createSSEParser } from '../service';

type FakePayload = {
  spaceKey: string;
  conversation: { id: string; peerId: string };
  message: { senderId: string; senderName: string; content: string };
};

/** 最小假宿主：只覆盖 parity 用例用到的 spark 面（openai/ollama 流式路径） */
function createFakeHost(streamChunks: Array<{ text: string }>) {
  const docsStore = new Map<string, Record<string, unknown>>();
  const dataStore = new Map<string, unknown>();
  let messageHandler: ((payload: FakePayload) => void) | undefined;
  const tokenChunks: string[] = [];
  let streamCounter = 0;

  const fake = {
    pluginId: 'ai-chat',
    onMessage: (fn: (payload: FakePayload) => void) => {
      messageHandler = fn;
    },
    onQuery: vi.fn(),
    ensureBot: vi.fn(),
    reply: vi.fn(),
    replyStreamStart: () => `stream-${++streamCounter}`,
    replyStreamChunk: (_payload: FakePayload, _messageId: string, text: string) => {
      tokenChunks.push(text);
    },
    replyStreamEnd: vi.fn(),
    log: vi.fn(),
    docs: {
      defineCollection: vi.fn(),
      get: (_collection: string, id: string) => docsStore.get(id) ?? null,
      query: () => ({
        items: [...docsStore.entries()].map(([id, data]) => ({ id, data })),
      }),
    },
    data: {
      declareCollection: vi.fn(),
      save: vi.fn(),
      get: (_name: string, key: string) => dataStore.get(key) ?? null,
    },
    sys: {
      exec: vi.fn(),
      fetch: vi.fn(),
      fetchStream: vi.fn(
        async (
          _url: string,
          _options?: unknown,
          onChunk?: (chunk: { text: string; done: boolean; status: number; headers: Record<string, string> }) => void,
        ) => {
          for (const c of streamChunks) {
            onChunk?.({ text: c.text, done: false, status: 200, headers: {} });
          }
          onChunk?.({ text: '', done: true, status: 200, headers: {} });
          return { text: '', done: true, status: 200, headers: {} };
        },
      ),
      execStream: vi.fn(),
    },
  };

  return {
    fake,
    tokenChunks,
    seedBot(id: string, doc: Record<string, unknown>) {
      docsStore.set(id, doc);
    },
    seedSecret(botId: string, apiKey: string) {
      dataStore.set(botId, { apiKey });
    },
    emit(botId: string, text: string) {
      messageHandler?.({
        spaceKey: 'personal',
        conversation: { id: `dm:bot:ai-chat:${botId}`, peerId: `bot:ai-chat:${botId}` },
        message: { senderId: 'me', senderName: '我', content: text },
      });
    },
  };
}

type FakeHost = ReturnType<typeof createFakeHost>;

async function loadBackground(host: FakeHost): Promise<void> {
  (globalThis as Record<string, unknown>).spark = host.fake;
  vi.resetModules();
  // @ts-expect-error background.ts 是 QuickJS 非模块脚本，动态 import 仅取顶层副作用
  await import('../background');
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
}

/** 后台侧跑一条消息，返回逐 chunk token 序列 */
async function backgroundTokens(
  botDoc: Record<string, unknown>,
  streamChunks: Array<{ text: string }>,
): Promise<string[]> {
  const host = createFakeHost(streamChunks);
  host.seedBot('b', botDoc);
  host.seedSecret('b', 'sk-test');
  await loadBackground(host);
  host.emit('b', 'hi');
  await flush();
  delete (globalThis as Record<string, unknown>).spark;
  return host.tokenChunks;
}

describe('后台（background.ts 内联）与 UI 侧（service.ts）解析器口径一致（评审 U2 锚定）', () => {
  beforeEach(() => {
    vi.resetModules();
  });
  afterEach(() => {
    delete (globalThis as Record<string, unknown>).spark;
  });

  it('SSE（OpenAI 兼容）：同一份含跨块断行的流，两侧 token 序列逐字一致', async () => {
    const wire = [
      'data: {"choices":[{"delta":{"content":"你',
      '"}}]}\n\ndata: {"choices":[{"delta":{"content":"好"}}]}\n\ndata: [DONE]\n\ndata: {"choices":[{"delta":{"content":"！"}}]}\n\n',
    ];
    const uiSeen: string[] = [];
    const push = createSSEParser((token) => uiSeen.push(token));
    for (const chunk of wire) push(chunk);

    const bgSeen = await backgroundTokens(
      {
        name: 'GPT',
        backendType: 'openai',
        backendConfig: { baseUrl: 'https://api.example.com/v1' },
        createdAt: 1,
      },
      wire.map((text) => ({ text })),
    );

    expect(bgSeen).toEqual(uiSeen);
    expect(uiSeen.join('')).toBe('你好！');
  });

  it('Ollama JSON-line：同一份含跨块断行的流，两侧 token 序列逐字一致', async () => {
    const wire = [
      '{"message":{"content":"你',
      '好"}}\n{"message":{"content":"！"}}\n{"done":true}\n',
    ];
    const uiSeen: string[] = [];
    const push = createOllamaStreamParser((token) => uiSeen.push(token));
    for (const chunk of wire) push(chunk);

    const bgSeen = await backgroundTokens(
      {
        name: 'Ollama',
        backendType: 'ollama',
        backendConfig: { endpoint: 'http://127.0.0.1:11434' },
        createdAt: 1,
      },
      wire.map((text) => ({ text })),
    );

    expect(bgSeen).toEqual(uiSeen);
    expect(uiSeen.join('')).toBe('你好！');
  });
});
