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
  /** data API（机密集合）存储：键 `${name}::${key}` */
  const dataStore = new Map<string, Map<string, unknown>>();
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
    // P6 声明式数据 API（机密集合走这里；与 PRELUDE 同步调用语义一致）
    data: {
      declareCollection: vi.fn(),
      save: (name: string, key: string, value: unknown) => {
        if (!dataStore.has(name)) dataStore.set(name, new Map());
        dataStore.get(name)!.set(key, value);
      },
      get: (name: string, key: string) => dataStore.get(name)?.get(key) ?? null,
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
    /** 预置机密集合中的 apiKey（模拟 UI 侧已拆存的状态） */
    seedSecret(botId: string, apiKey: string) {
      const coll = 'ai-chat:ai_chat_secrets';
      if (!dataStore.has(coll)) dataStore.set(coll, new Map());
      dataStore.get(coll)!.set(botId, { apiKey });
    },
    /** 读机密集合内容（断言用） */
    getSecret(botId: string): unknown {
      return dataStore.get('ai-chat:ai_chat_secrets')?.get(botId) ?? null;
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
  // workdir 必填（评审 S2）：未配置的 codebuddy bot 调用会被拒绝
  return { name: 'Echo Bot', backendType: 'codebuddy', backendConfig: { cliPath, workdir: 'C:/work' }, createdAt: 1 };
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

  it('历史遗留域不再由插件读取：内核迁移器在启动前已把旧档搬入自身域', async () => {
    // 评审 U1 · R3：四域探测随内核白名单一起退役。plugin: 根域 / 空间根域
    // 的存量 bot 文档由内核一次性迁移器（host_env/docs.rs LEGACY_DOC_MIGRATIONS）
    // 搬迁并删除；插件侧只读自身域，不再触碰任何遗留域
    host.seedBot('cb', codebuddyDoc(), 'space:personal');
    host.seedBot('cb2', codebuddyDoc(), 'plugin:ai-chat');
    await loadBackground(host);

    expect(host.calls.ensureBot).toEqual([]);
    expect(host.query('bot:query', { contactId: 'bot:ai-chat:cb' })).toEqual({ exists: false });
    // 自身域有数据时正常可见（迁移后的形态）
    host.seedBot('own', codebuddyDoc());
    expect(host.query('bot:query', { contactId: 'bot:ai-chat:own' })).toEqual({ exists: true });
  });

  it('codebuddy 后端：消息 → CLI（stream-json 流式）→ 回复', async () => {
    host.seedBot('cb', codebuddyDoc('C:/tools/codebuddy.exe'));
    await loadBackground(host);

    host.emit(messagePayload('cb', '帮我看看这段代码'));
    await flush();

    expect(host.fake.sys.execStream).toHaveBeenCalledWith(
      'C:/tools/codebuddy.exe',
      // 会话续接（评审 S5）：convId=dm:bot:ai-chat:cb → 一律先 --resume 续接
      ['--print', '--output-format', 'stream-json', '--include-partial-messages',
        '--resume', 'aichat-dm-bot-ai-chat-cb', '--', '帮我看看这段代码'],
      'C:/work',
      expect.any(Function)
    );
    expect(host.calls.streamStarts).toHaveLength(1);
    expect(host.calls.replies).toHaveLength(1);
    expect(host.calls.replies[0].text).toBe('REPLY-OK');
    expect(host.calls.replies[0].payload.conversation.id).toBe('dm:bot:ai-chat:cb');
  });

  it('codebuddy 未配置工作目录：拒绝调用并提示配置（评审 S2）', async () => {
    host.seedBot('cb', {
      name: 'No Dir Bot',
      backendType: 'codebuddy',
      backendConfig: { cliPath: 'codebuddy' },
      createdAt: 1,
    });
    await loadBackground(host);

    host.emit(messagePayload('cb', 'hi'));
    await flush();

    expect(host.fake.sys.execStream).not.toHaveBeenCalled();
    expect(host.fake.sys.exec).not.toHaveBeenCalled();
    const lastReply = host.calls.replies[host.calls.replies.length - 1].text;
    expect(lastReply).toContain('工作目录');
  });

  it('codebuddy resume 会话级失败：回退 --session-id 重建重试一次（评审 S5）', async () => {
    host.seedBot('cb', codebuddyDoc());
    // 第一次（--resume）：会话文件丢失，exitCode=1 且无 token 产出；
    // 第二次（--session-id 重建）：正常流式
    host.fake.sys.execStream.mockImplementationOnce(async (_p: string, _a?: string[], _w?: string, onChunk?: (c: { text: string; done: boolean; exitCode: number | null }) => void) => {
      onChunk?.({ text: '', done: true, exitCode: 1 });
      return { exitCode: 1, stdout: '', stderr: 'Error: session not found' };
    });
    await loadBackground(host);

    host.emit(messagePayload('cb', 'hi'));
    await flush();

    expect(host.fake.sys.execStream).toHaveBeenCalledTimes(2);
    const firstArgs = host.fake.sys.execStream.mock.calls[0][1] as string[];
    const secondArgs = host.fake.sys.execStream.mock.calls[1][1] as string[];
    expect(firstArgs).toContain('--resume');
    expect(secondArgs).toContain('--session-id');
    expect(host.calls.replies[host.calls.replies.length - 1].text).toBe('REPLY-OK');
  });

  it('codebuddy resume 已产出 token 的失败不回退重试（部分内容已上屏）', async () => {
    host.seedBot('cb', codebuddyDoc());
    host.fake.sys.execStream.mockImplementationOnce(async (_p: string, _a?: string[], _w?: string, onChunk?: (c: { text: string; done: boolean; exitCode: number | null }) => void) => {
      onChunk?.({
        text: '{"type":"assistant","message":{"content":[{"type":"text","text":"PART"}]}}',
        done: false,
        exitCode: null,
      });
      onChunk?.({ text: '', done: true, exitCode: 1 });
      return { exitCode: 1, stdout: '', stderr: 'boom' };
    });
    await loadBackground(host);

    host.emit(messagePayload('cb', 'hi'));
    await flush();

    expect(host.fake.sys.execStream).toHaveBeenCalledTimes(1);
    // 已产出 token 的流按干净终态收尾：已上屏的部分内容保留（fake 把逐 chunk
    // 折叠为一条 reply），不回退重发（重发会与已上屏内容重复）
    const lastReply = host.calls.replies[host.calls.replies.length - 1].text;
    expect(lastReply).toBe('PART');
  });

  it('超长/空消息：主聊天窗口路径拦截（评审 S3），不调用后端', async () => {
    host.seedBot('oa', {
      name: 'GPT Bot',
      backendType: 'openai',
      backendConfig: { baseUrl: 'https://api.example.com/v1' },
      createdAt: 1,
    });
    host.seedSecret('oa', 'sk-test');
    await loadBackground(host);

    host.emit(messagePayload('oa', 'x'.repeat(4001)));
    host.emit(messagePayload('oa', '   '));
    await flush();

    expect(host.fake.sys.fetchStream).not.toHaveBeenCalled();
    expect(host.fake.sys.fetch).not.toHaveBeenCalled();
    expect(host.calls.replies.map((r) => r.text)).toEqual([
      '消息长度不能超过 4000 字',
      '消息不能为空',
    ]);
  });

  it('孤儿 bot 告警日志只带摘要：不含消息内容与发送者昵称（评审 S1）', async () => {
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      await loadBackground(host);
      const payload = messagePayload('ghost', 'SECRET-CONTENT');
      payload.message.senderName = 'SECRET-NAME';
      host.emit(payload);
      await flush();

      expect(warnSpy).toHaveBeenCalled();
      const logged = warnSpy.mock.calls.map((c) => String(c[0])).join('\n');
      expect(logged).toContain('ghost');
      expect(logged).not.toContain('SECRET-CONTENT');
      expect(logged).not.toContain('SECRET-NAME');
    } finally {
      warnSpy.mockRestore();
    }
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
    // 会话一律先 --resume 续接（评审 S5）
    expect(host.fake.sys.execStream).toHaveBeenCalledWith(
      'codebuddy',
      ['--print', '--output-format', 'stream-json', '--include-partial-messages',
        '--resume', 'aichat-dm-bot-ai-chat-cb', '--', 'hi'],
      'D:/proj',
      expect.any(Function)
    );
    expect(host.calls.streamStarts).toHaveLength(1);
    expect(host.calls.replies[0].text).toBe('REPLY-OK');
  });

  it('openai 后端：消息 → /chat/completions（带鉴权与 system prompt）→ 回复', async () => {
    // 存量明文兼容：迁移完成前 apiKey 仍在 bot 文档里，后台兜底读取
    // （新写入的配置 key 只在本机机密集合，见下方专项用例）
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

  it('openai 后端：apiKey 从本机机密集合读取（同步文档不含 key）', async () => {
    host.seedBot('oa', {
      name: 'GPT Bot',
      backendType: 'openai',
      backendConfig: { baseUrl: 'https://api.example.com/v1', model: 'gpt-4o-mini' },
      createdAt: 1,
    });
    host.seedSecret('oa', 'sk-local-only');
    await loadBackground(host);

    host.emit(messagePayload('oa', '你好'));
    await flush();

    expect(host.fake.sys.fetchStream).toHaveBeenCalledWith(
      'https://api.example.com/v1/chat/completions',
      expect.objectContaining({
        headers: expect.objectContaining({ Authorization: 'Bearer sk-local-only' }),
      }),
      expect.any(Function)
    );
    expect(host.calls.replies[0].text).toBe('OPENAI-OK');
  });

  it('openai 后端：存量文档明文 key 兜底读取时顺手搬入机密集合', async () => {
    host.seedBot('oa', {
      name: 'GPT Bot',
      backendType: 'openai',
      backendConfig: { baseUrl: 'https://api.example.com/v1', apiKey: 'sk-legacy' },
      createdAt: 1,
    });
    await loadBackground(host);

    host.emit(messagePayload('oa', 'hi'));
    await flush();

    expect(host.fake.sys.fetchStream).toHaveBeenCalledWith(
      'https://api.example.com/v1/chat/completions',
      expect.objectContaining({
        headers: expect.objectContaining({ Authorization: 'Bearer sk-legacy' }),
      }),
      expect.any(Function)
    );
    //  Opportunistic 搬迁：key 已写入机密集合（UI 侧迁移随后清除文档字段）
    expect(host.getSecret('oa')).toEqual({ apiKey: 'sk-legacy' });
  });

  it('openai 后端：非 https 且非回环地址的 baseUrl 拒绝调用（key 不走明文链路）', async () => {
    host.seedBot('oa', {
      name: 'GPT Bot',
      backendType: 'openai',
      backendConfig: { baseUrl: 'http://api.example.com/v1' },
      createdAt: 1,
    });
    host.seedSecret('oa', 'sk-test');
    await loadBackground(host);

    host.emit(messagePayload('oa', 'hi'));
    await flush();

    expect(host.fake.sys.fetchStream).not.toHaveBeenCalled();
    expect(host.fake.sys.fetch).not.toHaveBeenCalled();
    const lastReply = host.calls.replies[host.calls.replies.length - 1].text;
    expect(lastReply).toContain('https');
  });

  it('openai 后端：回环地址的 http baseUrl 放行（本机服务场景）', async () => {
    host.seedBot('oa', {
      name: 'GPT Bot',
      backendType: 'openai',
      backendConfig: { baseUrl: 'http://127.0.0.1:8080/v1' },
      createdAt: 1,
    });
    host.seedSecret('oa', 'sk-test');
    await loadBackground(host);

    host.emit(messagePayload('oa', 'hi'));
    await flush();

    expect(host.fake.sys.fetchStream).toHaveBeenCalledWith(
      'http://127.0.0.1:8080/v1/chat/completions',
      expect.anything(),
      expect.any(Function)
    );
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
