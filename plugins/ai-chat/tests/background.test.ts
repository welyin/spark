/**
 * ai-chat 后台脚本（background.ts）的单元测试：
 * 用**假宿主**（内核 PRELUDE 注入的 spark API 的内存实现）驱动真实插件脚本，
 * 验证基本流程：启动注册联系人 → 消息路由后端 → 回复；宿主查询应答。
 *
 * 脚本经动态 import 执行（顶层副作用即「入口」）：每次用例前 resetModules +
 * 换新的假宿主，保证用例间互不影响。`spark` 在脚本中是自由变量，运行时
 * 解析到 globalThis.spark（与 QuickJS 沙箱的全局注入同构）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const BOTS_COLLECTION = 'ai_chat_bots';

type FakePayload = {
  spaceKey: string;
  conversation: { id: string; peerId: string };
  message: { senderId: string; senderName: string; content: string };
};

/** 假宿主：内核 plugin/runtime.rs PRELUDE 的 spark API 契约的内存实现 */
function createFakeHost() {
  const docsStore = new Map<string, Map<string, Record<string, unknown>>>();
  const calls = {
    ensureBot: [] as Array<{ botId: string; displayName: string }>,
    replies: [] as Array<{ payload: FakePayload; text: string }>,
    streamStarts: [] as FakePayload[],
    streamChunks: [] as Array<{ messageId: string; text: string }>,
    streamEnds: [] as Array<{ messageId: string; error?: string }>,
    logs: [] as string[],
  };
  let messageHandler: ((payload: FakePayload) => void) | undefined;
  const queryHandlers = new Map<string, (payload: unknown) => unknown>();

  /** docs 存储按域分桶：键 `${domain}::${collection}`（缺省域 = 插件 id） */
  const bucketKey = (domain: string | undefined, collection: string) =>
    `${domain ?? 'ai-chat'}::${collection}`;

  const fake = {
    pluginId: 'ai-chat',
    onMessage: (fn: (payload: FakePayload) => void) => {
      messageHandler = fn;
    },
    onQuery: (kind: string, fn: (payload: unknown) => unknown) => {
      queryHandlers.set(kind, fn);
    },
    ensureBot: (botId: string, displayName: string) => {
      calls.ensureBot.push({ botId, displayName });
      return `bot:ai-chat:${botId}`;
    },
    reply: (payload: FakePayload, text: string) => {
      calls.replies.push({ payload, text });
    },
    // 流式回复：fake 默认把 start/chunk/end 折叠为一条 reply（测试断言沿用
    // calls.replies 口径）；逐 chunk 行为由下方流式专项用例覆盖
    replyStreamStart: (payload: FakePayload) => {
      calls.streamStarts.push(payload);
      return `stream-${calls.streamStarts.length}`;
    },
    replyStreamChunk: (_payload: FakePayload, messageId: string, text: string) => {
      calls.streamChunks.push({ messageId, text });
    },
    replyStreamEnd: (payload: FakePayload, messageId: string, error?: string) => {
      calls.streamEnds.push({ messageId, error });
      // 折叠为一条 reply（与 UI 侧"流式完成=一条完整回复"口径一致）
      const content = calls.streamChunks
        .filter((c) => c.messageId === messageId)
        .map((c) => c.text)
        .join('');
      calls.replies.push({ payload, text: error ?? content });
    },
    log: (msg: string) => {
      calls.logs.push(msg);
    },
    docs: {
      defineCollection: vi.fn(),
      get: (collection: string, id: string, domain?: string) =>
        docsStore.get(bucketKey(domain, collection))?.get(id) ?? null,
      query: (collection: string, _options?: unknown, _config?: unknown, domain?: string) => {
        const coll = docsStore.get(bucketKey(domain, collection));
        const items = coll ? [...coll.entries()].map(([id, data]) => ({ id, data })) : [];
        return { items };
      },
    },
    sys: {
      exec: vi.fn(async () => ({ exitCode: 0, stdout: 'REPLY-OK', stderr: '' })),
      fetch: vi.fn(async () => ({
        status: 200,
        headers: {},
        body: JSON.stringify({ choices: [{ message: { content: 'OPENAI-OK' } }] }),
      })),
      // 流式：fake 推一段 SSE 数据（与真实 openai SSE 线形同构）后 done——
      // 验证 background 的 SSE 解析+逐 chunk 回复链路；断流/降级由专项用例注入
      fetchStream: vi.fn(
        async (
          _url: string,
          _options?: unknown,
          onChunk?: (chunk: { text: string; done: boolean; status: number; headers: Record<string, string> }) => void,
        ) => {
          onChunk?.({
            text: 'data: {"choices":[{"delta":{"content":"OPENAI-OK"}}]}\n\ndata: [DONE]\n\n',
            done: false,
            status: 200,
            headers: {},
          });
          onChunk?.({ text: '', done: true, status: 200, headers: {} });
          return { text: '', done: true, status: 200, headers: {} };
        },
      ),
      // codebuddy 流式：推真实 NDJSON 结构——assistant 事件（message.content[]
      // 的 type:"text" 块含增量）+ result 终态，后 done exitCode=0。
      // 验证 NDJSON 解析+逐 chunk 回复链路；未登录/异常用例在专项处覆盖此桩
      execStream: vi.fn(
        async (
          _program: string,
          _args?: string[],
          _workdir?: string,
          onChunk?: (chunk: { text: string; done: boolean; exitCode: number | null }) => void,
        ) => {
          onChunk?.({
            text: '{"type":"assistant","message":{"content":[{"type":"text","text":"REPLY-OK"}]}}',
            done: false,
            exitCode: null,
          });
          onChunk?.({
            text: '{"type":"result","result":"REPLY-OK"}',
            done: false,
            exitCode: null,
          });
          onChunk?.({ text: '', done: true, exitCode: 0 });
          return { exitCode: 0, stdout: '', stderr: '' };
        },
      ),
    },
  };

  return {
    fake,
    calls,
    /** 预置 bot 文档（与 UI 侧写入的字段口径一致；domain 缺省 = 插件自身域） */
    seedBot(id: string, doc: Record<string, unknown>, domain?: string) {
      const key = bucketKey(domain, BOTS_COLLECTION);
      if (!docsStore.has(key)) docsStore.set(key, new Map());
      docsStore.get(key)!.set(id, doc);
    },
    /** 模拟内核推送一条会话消息 */
    emit(payload: FakePayload) {
      messageHandler?.(payload);
    },
    /** 模拟宿主反向查询 */
    query(kind: string, payload: unknown) {
      return queryHandlers.get(kind)?.(payload);
    },
  };
}

type FakeHost = ReturnType<typeof createFakeHost>;

/** 加载后台脚本（顶层代码即入口：注册联系人 + 挂监听） */
async function loadBackground(host: FakeHost): Promise<void> {
  (globalThis as Record<string, unknown>).spark = host.fake;
  vi.resetModules();
  // @ts-expect-error background.ts 是 QuickJS 非模块脚本（零依赖约束，无
  // import/export），此处动态 import 仅取运行时顶层副作用，类型层面视作非模块
  await import('../background');
}

/** 冲刷脚本内的异步链（void async IIFE → await sys.exec/fetch → reply） */
async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 10));
}

function messagePayload(botId: string, text: string): FakePayload {
  return {
    spaceKey: 'personal',
    conversation: { id: `dm:bot:ai-chat:${botId}`, peerId: `bot:ai-chat:${botId}` },
    message: { senderId: 'me', senderName: '我', content: text },
  };
}

function codebuddyDoc(cliPath = 'codebuddy'): Record<string, unknown> {
  return { name: 'Echo Bot', backendType: 'codebuddy', backendConfig: { cliPath }, createdAt: 1 };
}

describe('ai-chat background script', () => {
  let host: FakeHost;

  beforeEach(() => {
    host = createFakeHost();
  });

  afterEach(() => {
    delete (globalThis as Record<string, unknown>).spark;
  });

  it('启动时为全部 bot 注册联系人', async () => {
    host.seedBot('bot-a', codebuddyDoc());
    host.seedBot('bot-b', { ...codebuddyDoc(), name: '二号 Bot' });
    await loadBackground(host);

    expect(host.calls.ensureBot).toEqual([
      { botId: 'bot-a', displayName: 'Echo Bot' },
      { botId: 'bot-b', displayName: '二号 Bot' },
    ]);
  });

  it('历史空间域兜底：bot 文档沉在 space:personal 域时仍能读写', async () => {
    // 旧 UI 桥把 docs 绑定到会话空间根域（历史缺陷），已有用户的 bot 数据在
    // space:personal——后台应兜底读到（注册 + 应答 + 查询都说 bot 存在）
    host.seedBot('cb', codebuddyDoc(), 'space:personal');
    await loadBackground(host);

    expect(host.calls.ensureBot).toEqual([{ botId: 'cb', displayName: 'Echo Bot' }]);
    expect(host.query('bot:query', { contactId: 'bot:ai-chat:cb' })).toEqual({ exists: true });

    host.emit(messagePayload('cb', 'hi'));
    await flush();
    expect(host.calls.replies[0].text).toBe('REPLY-OK');
  });

  it('自身域与历史域同时有数据：合并去重（自身域优先）', async () => {
    host.seedBot('a', codebuddyDoc());
    host.seedBot('b', { ...codebuddyDoc(), name: '旧 Bot' }, 'space:personal');
    await loadBackground(host);

    expect(host.calls.ensureBot.map((c) => c.botId)).toEqual(['a', 'b']);
  });

  it('codebuddy 后端：消息 → CLI（stream-json 流式）→ 回复', async () => {
    host.seedBot('cb', codebuddyDoc('C:/tools/codebuddy.exe'));
    await loadBackground(host);

    host.emit(messagePayload('cb', '帮我看看这段代码'));
    await flush();

    expect(host.fake.sys.execStream).toHaveBeenCalledWith(
      'C:/tools/codebuddy.exe',
      // 会话续接：convId=dm:bot:ai-chat:cb → 首次 --session-id 建立（含流式参数）
      ['--print', '--output-format', 'stream-json', '--include-partial-messages',
        '--session-id', 'aichat-dm-bot-ai-chat-cb', '--', '帮我看看这段代码'],
      undefined,
      expect.any(Function)
    );
    expect(host.calls.streamStarts).toHaveLength(1);
    expect(host.calls.replies).toHaveLength(1);
    expect(host.calls.replies[0].text).toBe('REPLY-OK');
    expect(host.calls.replies[0].payload.conversation.id).toBe('dm:bot:ai-chat:cb');
  });

  it('codebuddy 未登录：stderr 含 Authentication required 时回复登录指引', async () => {
    host.seedBot('cb', codebuddyDoc());
    // 流式路径：未登录时 stderr 经 execStream 终态返回（exitCode=1 + stderr）。
    // 流式终态失败时 background 以 error 收尾 replyStreamEnd（fake 折叠为一条
    // reply，text=error），断言登录指引文本经 error 通道透出
    host.fake.sys.execStream.mockImplementationOnce(async (_p: string, _a?: string[], _w?: string, onChunk?: (c: { text: string; done: boolean; exitCode: number | null }) => void) => {
      onChunk?.({ text: '', done: true, exitCode: 1 });
      return { exitCode: 1, stdout: '', stderr: 'Error: Authentication required, please use /login' };
    });
    await loadBackground(host);

    host.emit(messagePayload('cb', 'hi'));
    await flush();

    const lastReply = host.calls.replies[host.calls.replies.length - 1].text;
    expect(lastReply).toContain('尚未登录');
  });

  it('codebuddy 工作目录透传：配置 workdir 时作为第三参传给 sys.execStream（流式）', async () => {
    host.seedBot('cb', { ...codebuddyDoc(), backendConfig: { cliPath: 'codebuddy', workdir: 'D:/proj' } });
    await loadBackground(host);

    host.emit(messagePayload('cb', 'hi'));
    await flush();

    // 流式路径：codebuddy 走 execStream（stream-json），workdir 透传第三参；
    // 首次调用带 --session-id（会话续接建立）
    expect(host.fake.sys.execStream).toHaveBeenCalledWith(
      'codebuddy',
      ['--print', '--output-format', 'stream-json', '--include-partial-messages',
        '--session-id', 'aichat-dm-bot-ai-chat-cb', '--', 'hi'],
      'D:/proj',
      expect.any(Function)
    );
    expect(host.calls.streamStarts).toHaveLength(1);
    expect(host.calls.replies[0].text).toBe('REPLY-OK');
  });

  it('openai 后端：消息 → /chat/completions（带鉴权与 system prompt）→ 回复', async () => {
    host.seedBot('oa', {
      name: 'GPT Bot',
      backendType: 'openai',
      backendConfig: { baseUrl: 'https://api.example.com/v1', apiKey: 'sk-test', model: 'gpt-4o-mini' },
      systemPrompt: '你是测试助手',
      createdAt: 1,
    });
    await loadBackground(host);

    host.emit(messagePayload('oa', '你好'));
    await flush();

    // 流式路径：openai 走 fetchStream（非 fetch），body 带 stream:true
    expect(host.fake.sys.fetchStream).toHaveBeenCalledWith(
      'https://api.example.com/v1/chat/completions',
      expect.objectContaining({
        method: 'POST',
        headers: expect.objectContaining({ Authorization: 'Bearer sk-test' }),
      }),
      expect.any(Function)
    );
    const body = JSON.parse((host.fake.sys.fetchStream.mock.calls[0][1] as { body: string }).body);
    expect(body.model).toBe('gpt-4o-mini');
    expect(body.stream).toBe(true);
    expect(body.messages[0]).toEqual({ role: 'system', content: '你是测试助手' });
    expect(body.messages[1]).toEqual({ role: 'user', content: '你好' });
    // 流式回复折叠：SSE 解析出 OPENAI-OK 经逐 chunk 累积，end 折叠为一条 reply
    expect(host.calls.streamStarts).toHaveLength(1);
    expect(host.calls.replies[0].text).toBe('OPENAI-OK');
  });

  it('未知 bot 的消息：不调用后端也不回复（联系人孤儿）', async () => {
    await loadBackground(host);

    host.emit(messagePayload('ghost', 'hi'));
    await flush();

    expect(host.fake.sys.exec).not.toHaveBeenCalled();
    expect(host.fake.sys.fetch).not.toHaveBeenCalled();
    expect(host.calls.replies).toHaveLength(0);
  });

  it('宿主「bot 还在吗」查询：按 bot 列表应答 exists', async () => {
    host.seedBot('cb', codebuddyDoc());
    await loadBackground(host);

    expect(host.query('bot:query', { contactId: 'bot:ai-chat:cb' })).toEqual({ exists: true });
    expect(host.query('bot:query', { contactId: 'bot:ai-chat:ghost' })).toEqual({ exists: false });
    expect(host.query('bot:query', { contactId: 'not-a-bot' })).toEqual({ exists: false });
  });

  it('后端调用异常：回复错误提示而不是静默吞掉', async () => {
    host.seedBot('cb', codebuddyDoc());
    // 流式路径：execStream 启动即抛错（CLI 不存在等）→ catch 兜底回复错误提示
    host.fake.sys.execStream.mockRejectedValueOnce(new Error('启动命令失败'));
    await loadBackground(host);

    host.emit(messagePayload('cb', 'hi'));
    await flush();

    const lastReply = host.calls.replies[host.calls.replies.length - 1].text;
    expect(lastReply).toContain('调用 CodeBuddy CLI 失败');
  });
});
