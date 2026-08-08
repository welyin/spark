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

/** 宿主注入的后台 API（内核 plugin/runtime.rs PRELUDE 的声明镜像） */
declare const spark: {
  onMessage: (fn: (payload: SparkBackgroundMessage) => void) => void;
  onQuery: (kind: string, fn: (payload: unknown) => unknown) => void;
  /** 本插件 id（跨域共享数据的 key 空间；内核注入） */
  readonly pluginId: string;
  ensureBot: (botId: string, displayName: string) => string;
  reply: (payload: SparkBackgroundMessage, text: string) => unknown;
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

async function callCodebuddy(
  config: Record<string, unknown>,
  text: string
): Promise<BackendCallResult> {
  const cliPath = (config.cliPath as string) || 'codebuddy';
  // 工作目录：CLI 读取代码/文档上下文的根，用户显式配置
  const workdir = (config.workdir as string) || undefined;
  const startTime = Date.now();
  try {
    const result = await spark.sys.exec(cliPath, ['--print', text], workdir);
    const durationMs = Date.now() - startTime;
    const combined = [result.stdout, result.stderr].filter(Boolean).join('\n');
    if (/authentication required|please use \/login/i.test(combined)) {
      return {
        text: 'CodeBuddy CLI 尚未登录。\n\n请在终端中运行 codebuddy 进入交互模式，输入 /login 完成浏览器授权后，再回来对话。',
        durationMs,
      };
    }
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
  messages: Array<{ role: string; content: string }>
): Promise<BackendCallResult> {
  const baseUrl = config.baseUrl as string;
  const apiKey = config.apiKey as string;
  const model = (config.model as string) || 'gpt-4o';
  const startTime = Date.now();
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
  messages: Array<{ role: string; content: string }>
): Promise<BackendCallResult> {
  const endpoint = config.endpoint as string;
  const model = (config.model as string) || 'qwen2.5:7b';
  const startTime = Date.now();
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
async function handleBotMessage(bot: BotInstance, text: string): Promise<string> {
  const context: Array<{ role: string; content: string }> = [];
  if (bot.systemPrompt) {
    context.push({ role: 'system', content: bot.systemPrompt });
  }
  context.push({ role: 'user', content: text });

  let result: BackendCallResult;
  switch (bot.backendType) {
    case 'codebuddy':
      result = await callCodebuddy(bot.backendConfig, text);
      break;
    case 'openai':
      result = await callOpenai(bot.backendConfig, context);
      break;
    case 'ollama':
      result = await callOllama(bot.backendConfig, context);
      break;
    default:
      result = {
        text: '',
        durationMs: 0,
        error: `未注册的后端类型: "${bot.backendType as string}"`,
      };
  }
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
    console.warn(`[ai-chat][bg] 收到未知 bot 的消息 botId=${botId}（联系人孤儿），忽略`);
    return;
  }
  // 异步处理不阻塞事件循环：CLI/HTTP 调用期间后续消息仍可入队处理
  void (async () => {
    try {
      const response = await handleBotMessage(bot, payload.message.content ?? '');
      spark.reply(payload, response);
    } catch (err) {
      console.error('[ai-chat][bg] 消息处理失败:', err);
      try {
        spark.reply(payload, `处理失败：${err instanceof Error ? err.message : String(err)}`);
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
