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

/**
 * 用户消息最大长度（与 model.ts `MAX_MESSAGE_LENGTH` 逐字一致；零依赖约束故
 * 内联）。主聊天窗口路径的消息校验（评审 S3）：内核只有 16KiB 传输上限，
 * 语义校验（空消息 / 超长）由这里补齐。
 */
const MAX_MESSAGE_LENGTH = 4000;

/**
 * Bot 机密配置集合名（与 model.ts `SECRETS_COLLECTION` 逐字一致；零依赖约束故内联）。
 * 新数据 API，scope: local —— apiKey 不离开本机、不参与 pdsync 同步（评审 H1 · R1）。
 */
const SECRETS_COLLECTION = 'ai-chat:ai_chat_secrets';

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
  /** P6 声明式数据 API（同步调用语义；get 未命中返回 null） */
  data: {
    declareCollection: (decl: Record<string, unknown>) => Record<string, unknown>;
    save: (name: string, key: string, value: unknown) => void;
    get: (name: string, key: string) => unknown;
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
// Bot 实例读取（docs 域恒为插件 id——内核强制，历史缺陷域的存量文档已由
// 内核一次性迁移器在后台启动前搬入本域并删除旧档，见 host_env/docs.rs
// LEGACY_DOC_MIGRATIONS；配置每次现读，增删改即时生效）
// ------------------------------------------------------------------

/** 集合兜底声明（与 UI 侧 defineCollection 的口径一致；已持久化声明优先） */
const COLLECTION_CONFIG = { syncStrategy: 'lww', enableEvidence: false };

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
  const result = spark.docs.query(BOTS_COLLECTION, { reverse: true }, COLLECTION_CONFIG);
  return result.items.map(unwrapBotDoc);
}

function getBot(botId: string): BotInstance | null {
  ensureBotsCollection();
  const doc = spark.docs.get(BOTS_COLLECTION, botId);
  return doc ? unwrapBotDoc({ id: botId, data: doc }) : null;
}

// ------------------------------------------------------------------
// Bot 机密配置（apiKey）：local scope 机密集合现取（评审 H1 · R1.2）
// ------------------------------------------------------------------

/** 机密集合声明（与 UI 侧 service.ts ensureSecretsCollection 同口径） */
let secretsDeclared = false;
function ensureSecretsCollection(): void {
  if (secretsDeclared) return;
  try {
    spark.data.declareCollection({ name: SECRETS_COLLECTION, scope: 'local' });
  } catch {
    // 已声明则忽略（重复声明策略一致幂等，不一致才抛错——本插件策略恒定）
  }
  secretsDeclared = true;
}

/**
 * 取 bot 的 apiKey：优先本机机密集合；兜底存量明文文档（UI 侧迁移完成前的
 * 过渡期），读到旧位置明文时顺手搬入机密集合（UI 侧迁移随后会清除文档字段）。
 */
function loadBotApiKey(bot: BotInstance): string | undefined {
  ensureSecretsCollection();
  try {
    const rec = spark.data.get(SECRETS_COLLECTION, bot.id) as { apiKey?: string } | null;
    if (rec && typeof rec.apiKey === 'string' && rec.apiKey) return rec.apiKey;
  } catch {
    // 机密集合不可用则走存量明文兜底
  }
  const legacy = bot.backendConfig?.apiKey;
  if (typeof legacy === 'string' && legacy) {
    try {
      spark.data.save(SECRETS_COLLECTION, bot.id, { apiKey: legacy });
    } catch {
      // 搬迁失败不阻塞本次调用（下次消息仍会重试）
    }
    return legacy;
  }
  return undefined;
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
 * bot 会话 → codebuddy sessionId。codebuddy 把 session-id 当 `.jsonl` 文件名存盘，
 * Windows 下 `:` 是盘符/流分隔符、非法进文件名（实测 ENOENT）——尽管 --help 声称
 * `:` 合法（Unix 语义）。为跨平台安全，只保留字母数字 + `-` + `_`，其余全替换。
 */
function codebuddySessionId(convId: string): string {
  return `aichat-${convId.replace(/[^a-zA-Z0-9\-_]/g, '-')}`;
}

/** 未登录引导文本（stderr 命中 authentication 特征时的统一回复） */
const CODEBUDDY_AUTH_HINT =
  'CodeBuddy CLI 尚未登录。\n\n请在终端中运行 codebuddy 进入交互模式，输入 /login 完成浏览器授权后，再回来对话。';

/** 未配置工作目录时的拒绝文案（评审 S2；与 UI 侧 service.ts 同口径） */
const CODEBUDDY_WORKDIR_HINT =
  'CodeBuddy 后端未配置工作目录：未配置时 CLI 会继承宿主进程的当前目录（GUI 安装目录，读写不可控），' +
  '因此拒绝调用。请在插件中编辑该 Bot，填写「工作目录」（CLI 读取代码/文档上下文的根目录）后再试。';

/** 单次 codebuddy 调用结果 + 是否可回退重建会话重试 */
type CodebuddyAttempt = {
  result: BackendCallResult;
  /**
   * 会话级失败（exitCode 非 0 且非未登录）：--resume 续接的会话文件可能已
   * 丢失，调用方据此回退 --session-id 重建重试一次（评审 S5）
   */
  sessionFailure: boolean;
};

async function callCodebuddyOnce(
  cliPath: string,
  model: string | undefined,
  workdir: string,
  text: string,
  sessionArgs: string[],
  onToken?: StreamSink,
): Promise<CodebuddyAttempt> {
  const startTime = Date.now();

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
        return { result: { text: CODEBUDDY_AUTH_HINT, durationMs }, sessionFailure: false };
      }
      // 流式模式全文经 onToken 推送；exitCode 非 0 视为失败
      if (result.exitCode !== 0) {
        return {
          result: { text: '', durationMs, error: result.stderr || `codebuddy 退出码 ${result.exitCode}` },
          sessionFailure: true,
        };
      }
      return { result: { text: '', durationMs }, sessionFailure: false };
    } catch (err) {
      // spawn 失败（CLI 不存在等）：与会话状态无关，不回退重试
      return {
        result: {
          text: `调用 CodeBuddy CLI 失败：${err instanceof Error ? err.message : String(err)}`,
          durationMs: Date.now() - startTime,
        },
        sessionFailure: false,
      };
    }
  }

  try {
    // `--` 终止 CLI 选项解析：用户消息以 `-` 开头不会被误判为标志位。
    const baseArgs = ['--print', ...sessionArgs];
    const args = model ? ['--model', model, ...baseArgs, '--', text] : [...baseArgs, '--', text];
    const result = await spark.sys.exec(cliPath, args, workdir);
    const durationMs = Date.now() - startTime;
    const combined = [result.stdout, result.stderr].filter(Boolean).join('\n');
    if (/authentication required|please use \/login/i.test(combined)) {
      return { result: { text: CODEBUDDY_AUTH_HINT, durationMs }, sessionFailure: false };
    }
    // 与流式路径同口径：退出码非 0 视为失败（stderr 透出），不当作正常回复
    if (result.exitCode !== 0) {
      return {
        result: { text: '', durationMs, error: result.stderr || `codebuddy 退出码 ${result.exitCode}` },
        sessionFailure: true,
      };
    }
    return { result: { text: result.stdout || '(无输出)', durationMs }, sessionFailure: false };
  } catch (err) {
    return {
      result: {
        text: `调用 CodeBuddy CLI 失败：${err instanceof Error ? err.message : String(err)}`,
        durationMs: Date.now() - startTime,
      },
      sessionFailure: false,
    };
  }
}

/**
 * CodeBuddy CLI 后端。
 *
 * - 工作目录（评审 S2）：须用户显式配置；未配置拒绝调用（缺省会继承宿主
 *   进程 cwd——GUI 安装目录，CLI 读写不可控）。
 * - 会话续接（评审 S5）：同一 bot 会话绑定稳定 sessionId，一律先 `--resume`
 *   续接（codebuddy 自动带历史）；会话级失败且未产出任何 token 时回退
 *   `--session-id` 重建重试一次——无进程内/落盘状态、自愈，不再依赖
 *   「重启后重复 --session-id 幂等沿用原会话」的脆弱假设。无 convId
 *   （调用方未传）退化为无状态单轮。
 */
async function callCodebuddy(
  config: Record<string, unknown>,
  text: string,
  onToken?: StreamSink,
  convId?: string,
): Promise<BackendCallResult> {
  const cliPath = (config.cliPath as string) || 'codebuddy';
  const workdir = ((config.workdir as string) || '').trim();
  if (!workdir) {
    return { text: CODEBUDDY_WORKDIR_HINT, durationMs: 0 };
  }
  const model = (config.model as string) || undefined;

  const sessionId = convId ? codebuddySessionId(convId) : undefined;
  if (!sessionId) {
    return (await callCodebuddyOnce(cliPath, model, workdir, text, [], onToken)).result;
  }
  // 先 --resume；已产出 token 的失败不能重试（部分内容已上屏，重发会重复）
  let sawToken = false;
  const probe: StreamSink | undefined = onToken
    ? (token, accumulated) => {
        sawToken = true;
        onToken(token, accumulated);
      }
    : undefined;
  const attempt = await callCodebuddyOnce(cliPath, model, workdir, text, ['--resume', sessionId], probe);
  if (!attempt.sessionFailure || sawToken) return attempt.result;
  spark.log(`[ai-chat][bg] codebuddy resume 失败，回退 --session-id 重建会话 ${sessionId}`);
  return (await callCodebuddyOnce(cliPath, model, workdir, text, ['--session-id', sessionId], onToken)).result;
}

/**
 * OpenAI 类后端传输加固（与 model.ts validateBackendUrl 同口径，QuickJS 无 URL
 * 全局对象，零依赖约束故用正则内联）：强制 https；http 仅放行回环地址
 * （localhost/127.0.0.1/::1，本机 Ollama 类场景）——Bearer key 不走明文链路。
 */
function isSecureBackendUrl(url: string): boolean {
  if (/^https:\/\//i.test(url)) return true;
  return /^http:\/\/(localhost|127\.0\.0\.1|\[::1\]|::1)(:\d+)?(\/|$)/i.test(url);
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
  if (!isSecureBackendUrl(baseUrl ?? '')) {
    return {
      text: 'Base URL 必须使用 https（API Key 随请求头发送，明文 http 会泄露密钥）；仅 localhost/127.0.0.1/::1 本机服务允许 http。请在插件中修改该 Bot 的 Base URL。',
      durationMs: 0,
    };
  }
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
      // apiKey 不入同步文档：从本机机密集合现取合并（存量明文兜底见 loadBotApiKey）
      result = await callOpenai({ ...bot.backendConfig, apiKey: loadBotApiKey(bot) }, context, onToken);
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
    // 日志纪律（评审 S1）：孤儿消息告警只带摘要（botId/发送者 id/长度），
    // 消息内容与发送者昵称不进日志
    console.warn(
      `[ai-chat][bg] 收到未知 bot 的消息（孤儿），忽略 | botId=${botId} senderId=${payload.message.senderId} len=${(payload.message.content ?? '').length}`,
    );
    return;
  }
  // 消息校验（评审 S3，与 model.ts validateMessage 同口径）：主聊天窗口
  // 路径此前只靠内核 16KiB 传输上限，空/超长消息在这里拦截
  const content = payload.message.content ?? '';
  const trimmed = content.trim();
  if (!trimmed || trimmed.length > MAX_MESSAGE_LENGTH) {
    spark.reply(
      payload,
      !trimmed ? '消息不能为空' : `消息长度不能超过 ${MAX_MESSAGE_LENGTH} 字`,
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
