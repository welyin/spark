/**
 * ai-chat 插件 · 后台入口（内核 QuickJS 沙箱，plugin_system.md「后台运行时」）。
 *
 * 职责：
 * - 启动时为全部 bot 注册/刷新联系人（spark.ensureBot）；
 * - 监听发给本插件 bot 的会话消息（spark.onMessage，内核推送）→ 路由后端
 *   （CodeBuddy CLI / OpenAI 兼容 API / Ollama）→ 回复写入会话（spark.reply）；
 * - 应答宿主「bot 还在吗」查询（spark.onQuery，前端删除联系人守卫用）。
 *
 * 与旧后台 iframe 视图的架构差异：消息由内核按 bot 归属直接推入，不再有
 * 长轮询/10s 对账/配置签名——bot 配置在每条消息到达时现读（docs），
 * 增删改天然即时生效，也不存在「对账打断进行中回复」的吞消息窗口。
 *
 * 写作约束（QuickJS 沙箱，无 DOM/无 SDK 桥）：
 * - **零运行时依赖**：宿主直接 eval 本脚本，无法解析 ES module import，
 *   故连 model.ts 的常量也不 import（构建会把共享代码切 chunk）——集合名
 *   内联并保持与 model.ts 一致，类型本地声明；
 * - 宿主注入的全局只有 `spark`（下方声明）与 console；
 * - docs 域恒为本插件 id（内核强制），config 每次调用透传（与壳层 SDK 口径一致）。
 */

/** Bot 实例集合名（与 model.ts `BOTS_COLLECTION` 逐字一致；零依赖约束故内联） */
const BOTS_COLLECTION = 'ai_chat_bots';

// ── 与 model.ts 同形的类型（本地声明，避免运行时 import） ──

type BackendType = 'codebuddy' | 'openai' | 'ollama' | 'custom';

type BotInstance = {
  id: string;
  name: string;
  avatarUrl?: string;
  backendType: BackendType;
  backendConfig: Record<string, unknown>;
  systemPrompt?: string;
  createdAt: number;
};

type BackendCallResult = { text: string; durationMs: number; error?: string };

/** 会话消息载荷（与内核 ChatReceived 事件同构的子集） */
type SparkBackgroundMessage = {
  spaceKey: string;
  conversation: { id: string; peerId: string };
  message: { senderId: string; senderName: string; content: string };
};

type SparkExecResult = { exitCode: number; stdout: string; stderr: string };
type SparkFetchResult = { status: number; headers: Record<string, string>; body: string };
type SparkStreamChunk = { text: string; done: boolean; status: number; headers: Record<string, string> };
type SparkExecChunk = { text: string; done: boolean; exitCode: number | null };

/** 宿主注入的后台 API（内核 plugin/runtime.rs PRELUDE 的声明镜像） */
declare const spark: {
  onMessage: (fn: (payload: SparkBackgroundMessage) => void) => void;
  onQuery: (kind: string, fn: (payload: unknown) => unknown) => void;
  /** 本插件 id（跨域共享数据的 key 空间；内核注入） */
  readonly pluginId: string;
  ensureBot: (botId: string, displayName: string) => string;
  reply: (payload: SparkBackgroundMessage, text: string) => unknown;
  replyStreamStart: (payload: SparkBackgroundMessage) => string;
  replyStreamChunk: (payload: SparkBackgroundMessage, messageId: string, text: string) => void;
  replyStreamEnd: (payload: SparkBackgroundMessage, messageId: string, error?: string) => void;
  log: (msg: string) => void;
  docs: {
    get: (collection: string, id: string, domain?: string) => Record<string, unknown> | null;
    query: (
      collection: string,
      options?: unknown,
      config?: unknown,
      domain?: string
    ) => { items: Array<{ id: string; data: Record<string, unknown> }> };
    defineCollection: (collection: string, schema: unknown) => void;
  };
  sys: {
    exec: (program: string, args: string[], workdir?: string) => Promise<SparkExecResult>;
    fetch: (
      url: string,
      options?: { method?: string; headers?: Record<string, string>; body?: string }
    ) => Promise<SparkFetchResult>;
    fetchStream: (
      url: string,
      options?: { method?: string; headers?: Record<string, string>; body?: string },
      onChunk?: (chunk: SparkStreamChunk) => void
    ) => Promise<SparkStreamChunk>;
    execStream: (
      program: string,
      args?: string[],
      workdir?: string,
      onChunk?: (chunk: SparkExecChunk) => void
    ) => Promise<SparkExecResult>;
  };
};

// ------------------------------------------------------------------
// Bot 实例读取（docs 域 = 插件 id；配置每次现读，增删改即时生效）
// ------------------------------------------------------------------

/** 集合兜底声明（与 UI 侧 defineCollection 的口径一致；已持久化声明优先） */
const COLLECTION_CONFIG = { syncStrategy: 'lww', enableEvidence: false };

/**
 * 插件 bot 数据的 docs 域：
 * - 首选插件自身域（`spark.pluginId`，正常场景）；
 * - 兼容历史数据：旧 UI 桥曾把 docs 请求绑定到会话空间根域
 *   （`space:<spaceKey>`，壳层 derivePluginDomain 的历史缺陷），已有用户的
 *   bot 文档沉在那个域里——自身域查不到时回扫空间域兜底（两域是同一
 *   插件的可信数据面，不属于跨插件访问）。
 */
function botDataSpaces(): string[] {
  // 域候选：自身域（新数据）→ plugin: 根域（UI 桥历史数据面，存量 bot 文档
  // 的真实落点）→ 空间根域（更早的历史遗留）。逐域探测，有数据才纳入。
  const candidates = [
    spark.pluginId,
    `plugin:${spark.pluginId}`,
    'space:personal',
    'space:org',
  ];
  const active: string[] = [];
  const report: string[] = [];
  for (const domain of candidates) {
    try {
      const result = spark.docs.query(BOTS_COLLECTION, { limit: 1 }, COLLECTION_CONFIG, domain);
      report.push(`${domain}=${result.items.length > 0 ? '>=1' : '0'}`);
      if (result.items.length > 0 || domain === spark.pluginId) active.push(domain);
    } catch (e) {
      report.push(`${domain}=err`);
    }
  }
  console.log(`[ai-chat][bg] bot 数据域探测: ${report.join(' ')}`);
  return active;
}

/** 探测一次并缓存有效域列表（兜底域只承载历史数据的读） */
let cachedSpaces: string[] | null = null;
function dataSpaces(): string[] {
  if (!cachedSpaces) cachedSpaces = botDataSpaces();
  return cachedSpaces;
}

function ensureBotsCollection(): void {
  try {
    spark.docs.defineCollection(BOTS_COLLECTION, COLLECTION_CONFIG);
  } catch {
    // 已声明则忽略（defineCollection 重复声明会抛错）
  }
}

function unwrapBotDoc(doc: { id: string; data: Record<string, unknown> }): BotInstance {
  const d = doc.data;
  return {
    id: doc.id,
    name: (d.name as string) ?? 'Untitled Bot',
    avatarUrl: d.avatarUrl as string | undefined,
    backendType: (d.backendType as BackendType) ?? 'codebuddy',
    backendConfig: (d.backendConfig as Record<string, unknown>) ?? {},
    systemPrompt: d.systemPrompt as string | undefined,
    createdAt: (d.createdAt as number) ?? Date.now(),
  };
}

function listBots(): BotInstance[] {
  ensureBotsCollection();
  const seen = new Set<string>();
  const bots: BotInstance[] = [];
  for (const domain of dataSpaces()) {
    const result = spark.docs.query(BOTS_COLLECTION, { reverse: true }, COLLECTION_CONFIG, domain);
    for (const item of result.items) {
      if (seen.has(item.id)) continue;
      seen.add(item.id);
      bots.push(unwrapBotDoc(item));
    }
  }
  return bots;
}

function getBot(botId: string): BotInstance | null {
  ensureBotsCollection();
  for (const domain of dataSpaces()) {
    const doc = spark.docs.get(BOTS_COLLECTION, botId, domain);
    if (doc) return unwrapBotDoc({ id: botId, data: doc });
  }
  return null;
}

// ------------------------------------------------------------------
// 后端调用（config 字段与 UI 侧 service.ts 的 provider 口径一致）
// ------------------------------------------------------------------

/** 流式 token 回调（主聊天窗口逐字上屏用；undefined 表示不支持/未启用流式） */
type StreamSink = (token: string, accumulated: string) => void;

/** OpenAI SSE 流式解析（与 UI 侧 service.ts createSSEParser 同口径，零依赖约束故内联） */
function makeSsePush(onToken: StreamSink): (chunkText: string) => void {
  let buffer = '';
  let accumulated = '';
  return (chunkText: string) => {
    buffer += chunkText;
    const lines = buffer.split('\n');
    buffer = lines.pop() || '';
    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed || !trimmed.startsWith('data:')) continue;
      const data = trimmed.slice(5).trim();
      if (data === '[DONE]') continue;
      try {
        const parsed = JSON.parse(data);
        const token = parsed.choices?.[0]?.delta?.content;
        if (token) { accumulated += token; onToken(token, accumulated); }
      } catch { /* 残缺 JSON 跨块留待拼接 */ }
    }
  };
}

/** Ollama JSON-line 流式解析（同 UI 侧 createOllamaStreamParser 口径） */
function makeOllamaPush(onToken: StreamSink): (chunkText: string) => void {
  let buffer = '';
  let accumulated = '';
  return (chunkText: string) => {
    buffer += chunkText;
    const lines = buffer.split('\n');
    buffer = lines.pop() || '';
    for (const line of lines) {
      if (!line.trim()) continue;
      try {
        const parsed = JSON.parse(line);
        const token = parsed.message?.content;
        if (token) { accumulated += token; onToken(token, accumulated); }
      } catch { /* 残缺 JSON 跨块留待拼接 */ }
    }
  };
}

/**
 * CodeBuddy CLI stream-json（NDJSON）流式解析。两种模式（实测探测）：
 *
 * 真流式（`--include-partial-messages`，首选）——逐 token 增量：
 * - `stream_event.event.content_block_delta.delta`：`type:"text_delta"` 的
 *   `text` 字段是**逐 token 增量**（Hi / ! /  How can ...）——正文逐字；
 *   `type:"thinking_delta"` 的 `thinking` 字段是**思考过程增量**（"正在思考"
 *   内容，UI 可折叠展示）。
 * - 终态 `{"type":"result","result":"...全文..."}` 兜底（防增量漏字）。
 *
 * 快照回退（未加 partial-messages）——整段快照：
 * - `{"type":"assistant","message":{"content":[{"type":"text","text":"...整段..."}]}}`
 *   的 text 是截至当前的累积快照（非增量），按"取最新、推差量"处理。
 *
 * 内核 execStream 按完整行回调（行即完整 JSON），无需跨行缓冲。
 * onToken 是正文增量回调；thinking 经 onThinking 回调（可选，未传则忽略）。
 */
function makeCodebuddyPush(
  onToken: StreamSink,
  onThinking?: (delta: string, accumulated: string) => void,
): (chunkText: string) => void {
  let accumulated = '';       // 正文累计（text_delta 增量）
  let thinkingAcc = '';       // 思考过程累计（thinking_delta 增量）
  let snapshotAcc = '';       // 快照模式累计（assistant 整段，回退用）

  // 快照回退：text 快照按"取最新推差量"（不累加，防内容重复）
  const emitSnapshot = (snapshot: string) => {
    if (!snapshot) return;
    if (snapshot.startsWith(snapshotAcc)) {
      const delta = snapshot.slice(snapshotAcc.length);
      if (delta) {
        snapshotAcc = snapshot;
        accumulated = snapshot;
        onToken(delta, accumulated);
      }
    } else if (!snapshotAcc.startsWith(snapshot)) {
      snapshotAcc += snapshot;
      accumulated += snapshot;
      onToken(snapshot, accumulated);
    }
  };

  return (chunkText: string) => {
    if (!chunkText.trim()) return;
    try {
      const parsed = JSON.parse(chunkText);

      // 真流式：stream_event 增量（text_delta 正文 / thinking_delta 思考）
      if (parsed.type === 'stream_event') {
        const delta = parsed.event?.delta;
        if (delta?.type === 'text_delta' && typeof delta.text === 'string' && delta.text) {
          accumulated += delta.text;
          onToken(delta.text, accumulated);
        } else if (delta?.type === 'thinking_delta' && typeof delta.thinking === 'string' && delta.thinking) {
          thinkingAcc += delta.thinking;
          onThinking?.(delta.thinking, thinkingAcc);
        }
        return;
      }

      // 快照回退：assistant 整段（content 里 type:"text" 块）
      if (parsed.type === 'assistant') {
        const blocks = parsed.message?.content;
        if (Array.isArray(blocks)) {
          for (const block of blocks) {
            if (block?.type === 'text' && typeof block.text === 'string') {
              emitSnapshot(block.text);
            }
          }
        }
        return;
      }

      // 终态全文兜底：补齐与已见正文的差量
      if (parsed.type === 'result' && typeof parsed.result === 'string') {
        const full = parsed.result;
        if (full && full !== accumulated) {
          const delta = full.startsWith(accumulated) ? full.slice(accumulated.length) : full;
          if (delta) {
            accumulated = full;
            onToken(delta, accumulated);
          }
        }
      }
    } catch { /* 非 JSON 行（告警等）忽略 */ }
  };
}

/**
 * 已建立的 codebuddy 会话集合（codebuddy sessionId）。同一 bot 会话映射到同一
 * codebuddy 会话：首次用 --session-id 建立，后续 --resume 续接——codebuddy 自动
 * 携带完整历史（实测验证：resume 后 AI 记得前轮内容），实现"有记忆"的多轮对话。
 * 进程内记录；重启后若 sessionId 未在此集合会误判为首次走 --session-id，
 * codebuddy 对已存在的 session-id 重复建立是幂等的（沿用原会话），无害。
 */
const establishedSessions = new Set<string>();

/**
 * bot 会话 → codebuddy sessionId。codebuddy 把 session-id 当 `.jsonl` 文件名存盘，
 * Windows 下 `:` 是盘符/流分隔符、非法进文件名（实测 ENOENT）——尽管 --help 声称
 * `:` 合法（Unix 语义）。为跨平台安全，只保留字母数字 + `-` + `_`，其余全替换。
 */
function codebuddySessionId(convId: string): string {
  return `aichat-${convId.replace(/[^a-zA-Z0-9\-_]/g, '-')}`;
}

async function callCodebuddy(
  config: Record<string, unknown>,
  text: string,
  onToken?: StreamSink,
  convId?: string,
): Promise<BackendCallResult> {
  const cliPath = (config.cliPath as string) || 'codebuddy';
  // 工作目录：CLI 读取代码/文档上下文的根，用户显式配置
  const workdir = (config.workdir as string) || undefined;
  const model = (config.model as string) || undefined;
  const startTime = Date.now();

  // 会话续接：同一 bot 会话绑定稳定 sessionId，首次 --session-id 建立、后续
  // --resume 续接（codebuddy 自动带历史）。无 convId（调用方未传）退化为无状态单轮。
  const sessionId = convId ? codebuddySessionId(convId) : undefined;
  const sessionArgs: string[] = sessionId
    ? (establishedSessions.has(sessionId) ? ['--resume', sessionId] : ['--session-id', sessionId])
    : [];

  // 流式模式：`--output-format stream-json` 逐事件 NDJSON 输出，增量 token 在
  // stream_event.delta.text——主聊天窗口逐字上屏（sys.execStream 按行回流）。
  if (onToken) {
    try {
      // --include-partial-messages：开真流式（stream_event 逐 token 增量）——
      // 不加则只有整段 assistant 快照。思考过程（thinking_delta）也随此输出。
      const baseArgs = ['--print', '--output-format', 'stream-json', '--include-partial-messages', ...sessionArgs];
      const args = model ? ['--model', model, ...baseArgs, '--', text] : [...baseArgs, '--', text];
      const push = makeCodebuddyPush(onToken);
      spark.log('[stream-dbg] callCodebuddy execStream launch ' + cliPath);
      const result = await spark.sys.execStream(cliPath, args, workdir, (chunk) => {
        spark.log('[stream-dbg] cb chunk done=' + chunk.done + ' textLen=' + chunk.text.length);
        if (!chunk.done) push(chunk.text);
      });
      spark.log('[stream-dbg] cb execStream result exitCode=' + result.exitCode + ' stderr=' + result.stderr.slice(0, 80));
      const durationMs = Date.now() - startTime;
      if (/authentication required|please use \/login/i.test(result.stderr)) {
        return {
          text: 'CodeBuddy CLI 尚未登录。\n\n请在终端中运行 codebuddy 进入交互模式，输入 /login 完成浏览器授权后，再回来对话。',
          durationMs,
        };
      }
      // 流式模式全文经 onToken 推送；exitCode 非 0 视为失败
      if (result.exitCode !== 0) {
        return { text: '', durationMs, error: result.stderr || `codebuddy 退出码 ${result.exitCode}` };
      }
      if (sessionId) establishedSessions.add(sessionId); // 成功则标记已建立，后续 resume
      return { text: '', durationMs };
    } catch (err) {
      return {
        text: `调用 CodeBuddy CLI 失败：${err instanceof Error ? err.message : String(err)}`,
        durationMs: Date.now() - startTime,
      };
    }
  }

  try {
    // `--` 终止 CLI 选项解析：用户消息以 `-` 开头不会被误判为标志位。
    // 非流式分支同样带会话续接参数（保持"有记忆"一致）。
    const baseArgs = ['--print', ...sessionArgs];
    const args = model ? ['--model', model, ...baseArgs, '--', text] : [...baseArgs, '--', text];
    const result = await spark.sys.exec(cliPath, args, workdir);
    const durationMs = Date.now() - startTime;
    const combined = [result.stdout, result.stderr].filter(Boolean).join('\n');
    if (/authentication required|please use \/login/i.test(combined)) {
      return {
        text: 'CodeBuddy CLI 尚未登录。\n\n请在终端中运行 codebuddy 进入交互模式，输入 /login 完成浏览器授权后，再回来对话。',
        durationMs,
      };
    }
    if (sessionId && result.exitCode === 0) establishedSessions.add(sessionId);
    return { text: result.stdout || result.stderr || '(无输出)', durationMs };
  } catch (err) {
    return {
      text: `调用 CodeBuddy CLI 失败：${err instanceof Error ? err.message : String(err)}`,
      durationMs: Date.now() - startTime,
    };
  }
}

async function callOpenai(
  config: Record<string, unknown>,
  messages: Array<{ role: string; content: string }>,
  onToken?: StreamSink
): Promise<BackendCallResult> {
  const baseUrl = config.baseUrl as string;
  const apiKey = config.apiKey as string;
  const model = (config.model as string) || 'gpt-4o';
  const startTime = Date.now();
  // 流式模式：主聊天窗口逐字上屏
  if (onToken) {
    try {
      const push = makeSsePush(onToken);
      const done = await spark.sys.fetchStream(`${baseUrl}/chat/completions`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${apiKey}` },
        body: JSON.stringify({ model, messages, stream: true }),
      }, (chunk) => { if (!chunk.done) push(chunk.text); });
      if (done.status !== 200) {
        return { text: `调用 OpenAI 兼容 API 失败：HTTP ${done.status}`, durationMs: Date.now() - startTime };
      }
      return { text: '', durationMs: Date.now() - startTime }; // 全文经 onToken 推送
    } catch (err) {
      return {
        text: `调用 OpenAI 兼容 API 失败：${err instanceof Error ? err.message : String(err)}`,
        durationMs: Date.now() - startTime,
      };
    }
  }
  try {
    const result = await spark.sys.fetch(`${baseUrl}/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${apiKey}` },
      body: JSON.stringify({ model, messages }),
    });
    const data = JSON.parse(result.body);
    return { text: data.choices[0]?.message?.content ?? '(空响应)', durationMs: Date.now() - startTime };
  } catch (err) {
    return {
      text: `调用 OpenAI 兼容 API 失败：${err instanceof Error ? err.message : String(err)}`,
      durationMs: Date.now() - startTime,
    };
  }
}

async function callOllama(
  config: Record<string, unknown>,
  messages: Array<{ role: string; content: string }>,
  onToken?: StreamSink
): Promise<BackendCallResult> {
  const endpoint = config.endpoint as string;
  const model = (config.model as string) || 'qwen2.5:7b';
  const startTime = Date.now();
  if (onToken) {
    try {
      const push = makeOllamaPush(onToken);
      const done = await spark.sys.fetchStream(`${endpoint}/api/chat`, {
        method: 'POST',
        body: JSON.stringify({ model, messages, stream: true }),
      }, (chunk) => { if (!chunk.done) push(chunk.text); });
      if (done.status !== 200) {
        return { text: `调用 Ollama 失败：HTTP ${done.status}`, durationMs: Date.now() - startTime };
      }
      return { text: '', durationMs: Date.now() - startTime };
    } catch (err) {
      return {
        text: `调用 Ollama 失败：${err instanceof Error ? err.message : String(err)}`,
        durationMs: Date.now() - startTime,
      };
    }
  }
  try {
    const result = await spark.sys.fetch(`${endpoint}/api/chat`, {
      method: 'POST',
      body: JSON.stringify({ model, messages, stream: false }),
    });
    const data = JSON.parse(result.body);
    return { text: data.message?.content ?? '(空响应)', durationMs: Date.now() - startTime };
  } catch (err) {
    return {
      text: `调用 Ollama 失败：${err instanceof Error ? err.message : String(err)}`,
      durationMs: Date.now() - startTime,
    };
  }
}

/**
 * 路由后端并生成回复。主聊天窗口无插件聊天历史上下文，只传最后一条用户
 * 消息（与旧 handleMainChatMessage 口径一致）。
 */
async function handleBotMessage(bot: BotInstance, text: string, onToken?: StreamSink, convId?: string): Promise<string> {
  const context: Array<{ role: string; content: string }> = [];
  if (bot.systemPrompt) {
    context.push({ role: 'system', content: bot.systemPrompt });
  }
  context.push({ role: 'user', content: text });

  let result: BackendCallResult;
  switch (bot.backendType) {
    case 'codebuddy':
      result = await callCodebuddy(bot.backendConfig, text, onToken, convId);
      break;
    case 'openai':
      result = await callOpenai(bot.backendConfig, context, onToken);
      break;
    case 'ollama':
      result = await callOllama(bot.backendConfig, context, onToken);
      break;
    default:
      result = {
        text: '',
        durationMs: 0,
        error: `未注册的后端类型: "${bot.backendType as string}"`,
      };
  }
  // 流式模式 result.text 为空（全文经 onToken 推送）；非流式/CLI 直接返回
  return result.text || result.error || '（无响应）';
}

// ------------------------------------------------------------------
// 入口：注册联系人 + 消息监听 + 宿主查询应答
// ------------------------------------------------------------------

// bot 联系人注册（幂等：内核 contact_ensure_bot 对已存在联系人只刷新昵称）
for (const bot of listBots()) {
  try {
    spark.ensureBot(bot.id, bot.name);
  } catch (err) {
    console.error(`[ai-chat][bg] 注册 bot「${bot.name}」联系人失败:`, err);
  }
}

spark.onMessage((payload) => {
  // peerId 形如 bot:ai-chat:{botId}（内核已按此前缀路由，归属无需再校验）
  const peerId = payload.conversation.peerId;
  const botId = peerId.split(':').slice(2).join(':');
  if (!botId) return;
  const bot = getBot(botId);
  if (!bot) {
    console.warn(
      `[ai-chat][bg] 收到未知 bot 的消息（孤儿），忽略 | botId=${botId} sender=${payload.message.senderName} content="${payload.message.content}"`,
    );
    return;
  }
  // 异步处理不阻塞事件循环：CLI/HTTP 调用期间后续消息仍可入队处理
  void (async () => {
    // 流式回复：openai/ollama 走 fetchStream 逐 chunk 上屏；codebuddy CLI 无
    // 流式能力，仍走一次性 reply。流式路径：start 落占位 → 每 token chunk →
    // end 收尾；中途异常按 error 收尾（占位不残留）。
    const useStream =
      bot.backendType === 'openai' || bot.backendType === 'ollama' || bot.backendType === 'codebuddy';
    let streamId: string | null = null;
    let streamHadToken = false; // 流式是否真正产出过 token（区分正常流式 vs 后端业务失败）
    spark.log(`[stream-dbg] onMessage backendType=${bot.backendType} useStream=${useStream}`);
    try {
      if (useStream) {
        streamId = spark.replyStreamStart(payload);
        spark.log(`[stream-dbg] replyStreamStart ok streamId=${streamId}`);
      }
      // 内核 replyStreamChunk 是追加语义，sink 传增量 token（首个参）
      const sink: StreamSink | undefined = streamId
        ? (token) => {
            if (streamId) {
              streamHadToken = true;
              spark.replyStreamChunk(payload, streamId, token);
            }
          }
        : undefined;
      const response = await handleBotMessage(bot, payload.message.content ?? '', sink, payload.conversation.id);
      spark.log(`[stream-dbg] handleBotMessage done streamId=${streamId ?? 'none'} textLen=${response.length}`);
      if (streamId) {
        if (streamHadToken) {
          // 流式正常产出（有 token 上屏）→ 干净终态
          spark.replyStreamEnd(payload, streamId);
        } else {
          // 流式链路在但后端业务失败（未登录/退出码非0/空响应）：handleBotMessage
          // 返回错误文本而非抛异常——以 error 收尾，让占位消息显示该文本
          spark.replyStreamEnd(payload, streamId, response);
        }
      } else {
        spark.reply(payload, response);
      }
    } catch (err) {
      console.error('[ai-chat][bg] 消息处理失败:', err);
      const errText = `处理失败：${err instanceof Error ? err.message : String(err)}`;
      try {
        if (streamId) spark.replyStreamEnd(payload, streamId, errText);
        else spark.reply(payload, errText);
      } catch (replyErr) {
        console.error('[ai-chat][bg] 错误提示回写也失败:', replyErr);
      }
    }
  })();
});

// 宿主「bot 还在吗」查询（前端删除联系人守卫）：仍在插件 bot 列表 → 拦截删除
spark.onQuery('bot:query', (payload) => {
  const contactId = (payload as { contactId?: string })?.contactId ?? '';
  const botId = contactId.startsWith('bot:') ? contactId.split(':').slice(2).join(':') : '';
  if (!botId) return { exists: false };
  return { exists: listBots().some((b) => b.id === botId) };
});

spark.log(`ai-chat background started, ${listBots().length} bot(s) registered`);
