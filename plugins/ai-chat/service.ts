/**
 * ai-chat 插件 · 业务服务层。
 *
 * 职责：
 * - Bot 实例 CRUD（基于 docs 集合持久化）
 * - 聊天消息历史读写
 * - 后端提供者注册与路由
 * - 对话处理（接收用户消息 → 路由后端 → 写入 AI 回复）
 *
 * 依赖：sdk.docs 用于持久化，sdk.messages 用于发送应用消息。
 *
 * 待内核补充能力（sys.exec / sys.fetch）后，本文件中的后端 stub 实现
 * 替换为真实调用即可，接口签名不变。
 */

import type { PluginDocAPI, PluginMessagesAPI, PluginSDK } from '../../packages/plugin-sdk/src/index';
import { getPluginSDK } from '../../packages/plugin-sdk/src/index';
import type {
  BackendCallContext,
  BackendCallResult,
  BackendProvider,
  BackendType,
  BotInstance,
  ChatMessageRecord,
} from './model';
import {
  BOTS_COLLECTION,
  CHAT_HISTORY_COLLECTION,
} from './model';

// ------------------------------------------------------------------
// ID 生成
// ------------------------------------------------------------------

/** 生成带随机后缀的唯一 id（bot/消息等实体统一入口） */
export function generateId(prefix: string): string {
  const ts = Date.now().toString(36);
  const rand = Math.random().toString(36).slice(2, 8);
  return `${prefix}-${ts}-${rand}`;
}

// ------------------------------------------------------------------
// 后端提供者注册表
// ------------------------------------------------------------------

const backendProviders = new Map<BackendType, BackendProvider>();

/**
 * 注册后端提供者实现。
 * 在插件启动时由 index.ts 调用，注册所有可用的后端类型。
 */
export function registerBackendProvider(provider: BackendProvider): void {
  backendProviders.set(provider.type, provider);
}

/** 获取已注册的后端提供者 */
export function getBackendProvider(type: BackendType): BackendProvider | undefined {
  return backendProviders.get(type);
}

/** 获取所有可用后端类型 */
export function listAvailableBackendTypes(): BackendType[] {
  return Array.from(backendProviders.keys());
}

// ------------------------------------------------------------------
// 内置后端提供者注册
// ------------------------------------------------------------------

/**
 * 注册全部内置后端提供者（codebuddy / openai / ollama）。
 * 幂等（Map set 覆盖同名）。必须在主视图与后台视图两条入口都调用——
 * 后台视图不挂载 ChatView，若 provider 注册绑在视图组件里，后台监听
 * 调 handleMainChatMessage 时会报"未注册的后端类型"。
 */
export function registerBuiltinProviders(): void {
  // CodeBuddy CLI 后端
  registerBackendProvider({
    type: 'codebuddy',
    env: {
      candidateCommands: ['codebuddy', 'codebuddy-code', 'cbc'],
      probeArgs: ['--version'],
      installProgram: 'npm',
      installArgs: ['install', '-g', '@tencent-ai/codebuddy-code'],
      manualHint: 'npm install -g @tencent-ai/codebuddy-code',
      installedCommand: 'codebuddy',
    },
    call: async (ctx: BackendCallContext): Promise<BackendCallResult> => {
      const sdk = getPluginSDK();
      if (!sdk?.sys) return { text: '错误：内核 sys 能力不可用', durationMs: 0 };
      const cliPath = (ctx.config.cliPath as string) || 'codebuddy';
      const lastMsg = ctx.messages[ctx.messages.length - 1];
      // 工作目录：CLI 读取代码/文档上下文的根。用户显式配置；留空则 CLI 继承宿主
      // 进程 cwd（不可控），故强烈建议配置
      const workdir = (ctx.config.workdir as string) || undefined;
      console.log(`[ai-chat][provider] ctx.config=${JSON.stringify(ctx.config)}`);
      const startTime = Date.now();
      try {
        console.log(`[ai-chat][provider] sys.exec 调用 cliPath=${cliPath} workdir=${workdir ?? '(继承)'}`);
        const result = await sdk.sys.exec(cliPath, ['--print', lastMsg?.content ?? ''], workdir);
        console.log(`[ai-chat][provider] sys.exec 返回 exitCode=${result.exitCode} stdout=${result.stdout?.slice(0, 50) ?? ''}`);
        const durationMs = Date.now() - startTime;
        const combined = [result.stdout, result.stderr].filter(Boolean).join('\n');
        if (/authentication required|please use \/login/i.test(combined)) {
          return {
            text: 'CodeBuddy CLI 尚未登录。\n\n请在终端中运行 codebuddy 进入交互模式，输入 /login 完成浏览器授权后，再回来对话。',
            durationMs,
          };
        }
        const text = result.stdout || result.stderr || '(无输出)';
        return { text, durationMs };
      } catch (err) {
        const durationMs = Date.now() - startTime;
        return { text: `调用 CodeBuddy CLI 失败：${err instanceof Error ? err.message : String(err)}`, durationMs };
      }
    },
  });

  // OpenAI 兼容 API 后端
  registerBackendProvider({
    type: 'openai',
    call: async (ctx: BackendCallContext): Promise<BackendCallResult> => {
      const sdk = getPluginSDK();
      if (!sdk?.sys) return { text: '错误：内核 sys 能力不可用', durationMs: 0 };
      const baseUrl = ctx.config.baseUrl as string;
      const apiKey = ctx.config.apiKey as string;
      const model = (ctx.config.model as string) || 'gpt-4o';
      const messages = ctx.messages.map((m) => ({ role: m.role, content: m.content }));
      const startTime = Date.now();
      try {
        const result = await sdk.sys.fetch(`${baseUrl}/chat/completions`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${apiKey}` },
          body: JSON.stringify({ model, messages }),
        });
        const durationMs = Date.now() - startTime;
        const data = JSON.parse(result.body);
        return { text: data.choices[0]?.message?.content ?? '(空响应)', durationMs };
      } catch (err) {
        const durationMs = Date.now() - startTime;
        return { text: `调用 OpenAI 兼容 API 失败：${err instanceof Error ? err.message : String(err)}`, durationMs };
      }
    },
  });

  // Ollama 本地模型后端
  registerBackendProvider({
    type: 'ollama',
    call: async (ctx: BackendCallContext): Promise<BackendCallResult> => {
      const sdk = getPluginSDK();
      if (!sdk?.sys) return { text: '错误：内核 sys 能力不可用', durationMs: 0 };
      const endpoint = ctx.config.endpoint as string;
      const model = (ctx.config.model as string) || 'qwen2.5:7b';
      const messages = ctx.messages.map((m) => ({ role: m.role, content: m.content }));
      const startTime = Date.now();
      try {
        const result = await sdk.sys.fetch(`${endpoint}/api/chat`, {
          method: 'POST',
          body: JSON.stringify({ model, messages, stream: false }),
        });
        const durationMs = Date.now() - startTime;
        const data = JSON.parse(result.body);
        return { text: data.message?.content ?? '(空响应)', durationMs };
      } catch (err) {
        const durationMs = Date.now() - startTime;
        return { text: `调用 Ollama 失败：${err instanceof Error ? err.message : String(err)}`, durationMs };
      }
    },
  });
}

// ------------------------------------------------------------------
// 后端环境检测与一键安装（仅命令行类后端需要）
// ------------------------------------------------------------------

/** 单个候选命令的探测结果 */
export type EnvCandidate = {
  /** 探测到的可执行命令/路径（用户选择后写入 bot 配置 cliPath） */
  command: string;
  /** 来源：PATH 探测到的命令名，或用户手动填写的路径 */
  source: 'path' | 'manual';
};

/** 环境扫描结果 */
export type BackendEnvScan =
  | { state: 'unsupported' } // 纯 HTTP 后端，无本地依赖
  | { state: 'found'; candidates: EnvCandidate[] } // 检测到 ≥1 个可用命令
  | { state: 'missing'; manualHint: string }; // 一个都没检测到

/** 探测单个命令是否可用（执行 probe 子命令，exitCode 0 即就绪）。
 *  失败原因打到 console，便于定位"找不到命令"的真实环节（sys 未注入 /
 *  spawn 失败 / 命令退出码非 0 / stderr 内容） */
async function probeCommand(sdk: PluginSDK, command: string, probeArgs: string[]): Promise<boolean> {
  if (!sdk.sys) {
    console.warn(`[ai-chat][probe] sdk.sys 未注入，无法探测 ${command}`);
    return false;
  }
  try {
    const result = await sdk.sys.exec(command, probeArgs);
    if (result.exitCode !== 0) {
      console.warn(
        `[ai-chat][probe] ${command} 退出码=${result.exitCode} stderr=${result.stderr?.slice(0, 200) ?? ''}`
      );
    }
    return result.exitCode === 0;
  } catch (err) {
    console.warn(`[ai-chat][probe] ${command} spawn 失败:`, err);
    return false;
  }
}

/** npm 全局 bin 目录缓存（进程内只探测一次） */
let npmGlobalBinCache: string | null | undefined;

/**
 * 探测 npm 全局安装目录（`npm config get prefix`）。
 * Windows 下 npm i -g 的 shim 直接放在 prefix 根目录。
 * 用于 PATH 未刷新（GUI 进程 PATH 是启动时快照）时定位新装命令的绝对路径。
 */
async function npmGlobalBinDir(sdk: PluginSDK): Promise<string | null> {
  if (npmGlobalBinCache !== undefined) return npmGlobalBinCache;
  npmGlobalBinCache = null;
  if (!sdk.sys) return null;
  try {
    const r = await sdk.sys.exec('npm', ['config', 'get', 'prefix']);
    const prefix = r.stdout.trim();
    if (r.exitCode === 0 && prefix) {
      npmGlobalBinCache = prefix;
    } else {
      console.warn(`[ai-chat][probe] npm config get prefix 失败: 退出码=${r.exitCode} stderr=${r.stderr?.slice(0, 200) ?? ''}`);
    }
  } catch (err) {
    console.warn('[ai-chat][probe] npm config get prefix spawn 失败:', err);
  }
  return npmGlobalBinCache;
}

/**
 * 探测单个命令：先按裸命令名（依赖 PATH），失败则解析 npm 全局目录的
 * 绝对路径再试。返回实际可用的命令字符串（裸名或绝对路径），不可用返回 null。
 */
async function resolveUsableCommand(
  sdk: PluginSDK,
  command: string,
  probeArgs: string[],
): Promise<string | null> {
  // 裸命令名优先（PATH 可用时最干净）
  if (await probeCommand(sdk, command, probeArgs)) {
    return command;
  }
  // PATH 未刷新兜底：拼 npm 全局 shim 绝对路径（Windows .cmd / POSIX 无后缀）
  const binDir = await npmGlobalBinDir(sdk);
  if (binDir) {
    const isWindows = binDir.includes('\\') || /^[A-Za-z]:/.test(binDir);
    const abs = isWindows ? `${binDir}\\${command}.cmd` : `${binDir}/bin/${command}`;
    if (await probeCommand(sdk, abs, probeArgs)) {
      return abs;
    }
  }
  return null;
}

/**
 * 扫描某后端类型的本地环境：逐个探测候选命令（含 npm 全局目录兜底）。
 * - 全部未命中 → missing（引导手动填路径或一键安装）
 * - 命中 ≥1 个 → found（单命中直接用，多命中让用户选）
 */
export async function scanBackendEnv(sdk: PluginSDK, type: BackendType): Promise<BackendEnvScan> {
  const provider = getBackendProvider(type);
  if (!provider?.env) {
    return { state: 'unsupported' };
  }
  const hits: EnvCandidate[] = [];
  for (const cmd of provider.env.candidateCommands) {
    const usable = await resolveUsableCommand(sdk, cmd, provider.env.probeArgs);
    if (usable) {
      hits.push({ command: usable, source: usable === cmd ? 'path' : 'manual' });
    }
  }
  if (hits.length > 0) {
    return { state: 'found', candidates: hits };
  }
  return { state: 'missing', manualHint: provider.env.manualHint };
}

/**
 * 校验用户手动填写的可执行路径/命令是否可用。
 * 可用则返回该候选（source='manual'），否则返回 null。
 */
export async function verifyManualCommand(
  sdk: PluginSDK,
  type: BackendType,
  command: string,
): Promise<EnvCandidate | null> {
  const provider = getBackendProvider(type);
  if (!provider?.env) return null;
  const trimmed = command.trim();
  if (!trimmed) return null;
  // 含路径分隔符的视为绝对/相对路径直接 spawn 校验；裸命令名走 npm 全局兜底
  const usable = trimmed.includes('\\') || trimmed.includes('/')
    ? (await probeCommand(sdk, trimmed, provider.env.probeArgs) ? trimmed : null)
    : await resolveUsableCommand(sdk, trimmed, provider.env.probeArgs);
  return usable ? { command: usable, source: 'manual' } : null;
}

/**
 * 一键安装某后端类型的本地 CLI（如 `npm install -g @tencent-ai/codebuddy-code`）。
 * 安装成功后用 installedCommand 复检；返回 { ok, output, command }，
 * command 为安装成功后可用的命令名（供写入 bot 配置）。
 */
export async function installBackendEnv(
  sdk: PluginSDK,
  type: BackendType,
): Promise<{ ok: boolean; output: string; command?: string }> {
  const provider = getBackendProvider(type);
  if (!provider?.env) {
    return { ok: false, output: '该后端无需本地安装' };
  }
  if (!sdk.sys) {
    return { ok: false, output: '内核 sys 能力不可用，无法自动安装' };
  }
  try {
    const result = await sdk.sys.exec(provider.env.installProgram, provider.env.installArgs);
    const output = [result.stdout, result.stderr].filter(Boolean).join('\n');
    if (result.exitCode !== 0) {
      return { ok: false, output: output || '(无输出)' };
    }
    // 安装成功：先按命令名复检（PATH 已含 npm 全局目录时直接可用）
    const usable = await probeCommand(sdk, provider.env.installedCommand, provider.env.probeArgs);
    if (usable) {
      return { ok: true, output: output || '(安装成功)', command: provider.env.installedCommand };
    }
    // PATH 未刷新（GUI 进程 PATH 是启动时快照，npm i -g 刚装的 shim 还搜不到）：
    // 复用 resolveUsableCommand 的 npm 全局目录兜底，定位绝对路径
    const usablePath = await resolveUsableCommand(sdk, provider.env.installedCommand, provider.env.probeArgs);
    if (usablePath) {
      return { ok: true, output: output || '(安装成功)', command: usablePath };
    }
    return { ok: false, output: `${output}\n安装后未能定位 ${provider.env.installedCommand}，请尝试手动填写路径` };
  } catch (err) {
    return { ok: false, output: err instanceof Error ? err.message : String(err) };
  }
}

// ------------------------------------------------------------------
// Bot 实例管理
// ------------------------------------------------------------------

/**
 * 文档键转为 BotInstance 的规范化：
 * docs.put() 将 id 并入文档体存储，docs.get() / query() 返回时
 * 外层 { id, data } 包裹——这里从包裹结构中拆出。
 */
function unwrapBotDoc(doc: { id: string; data: Record<string, unknown> }): BotInstance {
  const d = doc.data as Record<string, unknown>;
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

function wrapBotDoc(bot: BotInstance): Record<string, unknown> {
  return {
    name: bot.name,
    avatarUrl: bot.avatarUrl,
    backendType: bot.backendType,
    backendConfig: bot.backendConfig,
    systemPrompt: bot.systemPrompt,
    createdAt: bot.createdAt,
  };
}

/** 确保集合已声明（首次写入前调用） */
async function ensureBotsCollection(docsApi: PluginDocAPI): Promise<void> {
  try {
    await docsApi.defineCollection(BOTS_COLLECTION, {
      syncStrategy: 'lww',
      enableEvidence: false,
    });
  } catch {
    // 已声明则静默忽略（defineCollection 重复声明会抛错）
  }
}

/** 列出所有 Bot 实例 */
export async function listBots(docsApi: PluginDocAPI): Promise<BotInstance[]> {
  await ensureBotsCollection(docsApi);
  const result = await docsApi.query(BOTS_COLLECTION, { reverse: true });
  return result.items.map(unwrapBotDoc);
}

/** 获取单个 Bot 实例 */
export async function getBot(
  docsApi: PluginDocAPI,
  botId: string,
): Promise<BotInstance | null> {
  await ensureBotsCollection(docsApi);
  const doc = await docsApi.get<Record<string, unknown>>(BOTS_COLLECTION, botId);
  if (!doc) {
    return null;
  }
  return unwrapBotDoc({ id: botId, data: doc });
}

/** 创建 Bot 实例 */
export async function createBot(
  docsApi: PluginDocAPI,
  bot: Omit<BotInstance, 'createdAt'>,
): Promise<BotInstance> {
  await ensureBotsCollection(docsApi);
  const instance: BotInstance = {
    ...bot,
    createdAt: Date.now(),
  };
  await docsApi.put(BOTS_COLLECTION, bot.id, wrapBotDoc(instance));
  return instance;
}

/** 更新 Bot 实例 */
export async function updateBot(
  docsApi: PluginDocAPI,
  botId: string,
  patch: Partial<Omit<BotInstance, 'id' | 'createdAt'>>,
): Promise<BotInstance | null> {
  const existing = await getBot(docsApi, botId);
  if (!existing) {
    return null;
  }
  const updated: BotInstance = {
    ...existing,
    ...patch,
  };
  await docsApi.put(BOTS_COLLECTION, botId, wrapBotDoc(updated));
  return updated;
}

/** 删除 Bot 实例 */
export async function deleteBot(
  docsApi: PluginDocAPI,
  botId: string,
): Promise<boolean> {
  await ensureBotsCollection(docsApi);
  await docsApi.delete(BOTS_COLLECTION, botId);
  return true;
}

// ------------------------------------------------------------------
// 聊天历史管理
// ------------------------------------------------------------------

async function ensureChatHistoryCollection(docsApi: PluginDocAPI): Promise<void> {
  try {
    await docsApi.defineCollection(CHAT_HISTORY_COLLECTION, {
      syncStrategy: 'lww',
      enableEvidence: false,
    });
  } catch {
    // 已声明
  }
}

/** 获取指定 Bot 实例的聊天历史 */
export async function listChatMessages(
  docsApi: PluginDocAPI,
  botInstanceId: string,
  limit = 50,
): Promise<ChatMessageRecord[]> {
  await ensureChatHistoryCollection(docsApi);
  const result = await docsApi.query(CHAT_HISTORY_COLLECTION, {
    limit,
    filter: [
      { field: 'botInstanceId', value: botInstanceId },
    ],
  });
  return result.items.map((item) => item.data as unknown as ChatMessageRecord);
}

/** 写入一条聊天消息 */
export async function saveChatMessage(
  docsApi: PluginDocAPI,
  msg: ChatMessageRecord,
): Promise<void> {
  await ensureChatHistoryCollection(docsApi);
  await docsApi.put(CHAT_HISTORY_COLLECTION, msg.id, msg as unknown as Record<string, unknown>);
}

// ------------------------------------------------------------------
// 后端调用路由
// ------------------------------------------------------------------

/**
 * 路由到对应后端提供者并调用。
 * 返回 AI 回复结果（文本 + 耗时），出错时附带 error 信息。
 */
async function callBackend(
  bot: BotInstance,
  messages: ChatMessageRecord[],
): Promise<BackendCallResult> {
  const provider = backendProviders.get(bot.backendType);
  if (!provider) {
    return {
      text: '',
      durationMs: 0,
      error: `未注册的后端类型: "${bot.backendType}"。请确认后端提供者已注册。`,
    };
  }

  const ctx: BackendCallContext = {
    config: bot.backendConfig,
    messages,
  };

  return provider.call(ctx);
}

// ------------------------------------------------------------------
// 对话处理（核心流程）
// ------------------------------------------------------------------

/** 处理对话的结果（返回给视图层） */
export type ProcessChatResult = {
  /** 已持久化的用户消息 */
  userMessage: ChatMessageRecord;
  /** AI 回复（可能为空，如调用失败） */
  assistantMessage: ChatMessageRecord | null;
  /** 调用后端时发生的错误（null 表示成功） */
  error?: string;
};

/**
 * 处理一条用户对话消息：
 * 1. 保存用户消息
 * 2. 加载该 Bot 的历史消息构建上下文
 * 3. 调用后端
 * 4. 保存 AI 回复
 */
export async function processChat(
  docsApi: PluginDocAPI,
  bot: BotInstance,
  userContent: string,
): Promise<ProcessChatResult> {
  // 1. 保存用户消息
  const now = Date.now();
  const userMsg: ChatMessageRecord = {
    id: generateId('msg'),
    botInstanceId: bot.id,
    role: 'user',
    content: userContent,
    createdAt: now,
  };
  await saveChatMessage(docsApi, userMsg);

  // 2. 加载历史 + 构建上下文消息列表
  const history = await listChatMessages(docsApi, bot.id);

  // 按时间排序并过滤出相关消息
  const contextMessages: ChatMessageRecord[] = [];
  if (bot.systemPrompt) {
    contextMessages.push({
      id: generateId('sys'),
      botInstanceId: bot.id,
      role: 'system',
      content: bot.systemPrompt,
      createdAt: 0,
    });
  }
  for (const msg of history) {
    // 当前用户消息已在上方保存，跳过（避免重复）
    if (msg.id === userMsg.id) {
      continue;
    }
    if (msg.role === 'user' || msg.role === 'assistant') {
      contextMessages.push(msg);
    }
  }
  contextMessages.push(userMsg);

  // 3. 调用后端
  const startTime = Date.now();
  const result = await callBackend(bot, contextMessages);
  const durationMs = result.durationMs > 0 ? result.durationMs : Date.now() - startTime;

  // 4. 构建 + 保存 AI 回复
  const assistantMsg: ChatMessageRecord = {
    id: generateId('msg'),
    botInstanceId: bot.id,
    role: 'assistant',
    content: result.text || result.error || '（无响应）',
    createdAt: Date.now(),
    durationMs,
    error: result.error,
  };
  await saveChatMessage(docsApi, assistantMsg);

  // 5. 不再发送服务号应用消息：bot 已是联系人，回复只应出现在联系人会话里，
  // sendAppMessage 是服务号模型的误用，会制造 app:ai-chat 噪音会话。
  // （插件内聊天与联系人会话的数据合并是独立重构，另行处理）

  return {
    userMessage: userMsg,
    assistantMessage: assistantMsg,
    error: result.error,
  };
}

// ------------------------------------------------------------------
// Bot 联系人（适配 Spark 主聊天窗口）
// ------------------------------------------------------------------

/** 构造 bot 的 rootId（约定格式：bot:<pluginId>:<botId>） */
export function botRootId(botId: string): string {
  return `bot:ai-chat:${botId}`;
}

/** 将 bot 注册为 Spark 联系人（使其出现在通讯录中） */
export async function registerBotContact(
  messages: PluginMessagesAPI,
  botId: string,
  displayName: string,
): Promise<void> {
  await messages.registerAsContact(botRootId(botId), displayName);
}

/** 结束 bot 消息处理循环的 token */
type StopToken = { stop: () => void };

/**
 * 启动 bot 主聊天消息监听循环。
 * 通过长轮询接收用户从 Spark 主聊天窗口发送给 bot 的消息，
 * 调用后端生成回复并通过 sendResponse 写入会话。
 *
 * 返回 StopToken，调用 `stop()` 可终止循环。
 */
export function startBotMessageProcessor(
  sdk: PluginSDK,
  bot: BotInstance,
): StopToken {
  const messagesApi = sdk.messages;
  if (!messagesApi) {
    console.warn('[ai-chat] messages 模块不可用，跳过消息监听');
    return { stop: () => {} };
  }

  let running = true;
  const rootId = botRootId(bot.id);

  async function poll(): Promise<void> {
    while (running) {
      try {
        const event = await messagesApi!.waitForMessage(rootId, 30000);
        if (!event) continue;               // 超时无消息，继续轮询
        if (!running) break;

        // 调用后端处理
        console.log(`[ai-chat][bg] 收到消息 bot=${bot.id} text=${event.text?.slice(0, 30)}，调用后端`);
        let response: string;
        try {
          response = await handleMainChatMessage(bot, event.text);
          console.log(`[ai-chat][bg] handleMainChatMessage resolve，response 长度=${response?.length ?? 'undefined'}`);
        } catch (backendErr) {
          console.error('[ai-chat][bg] handleMainChatMessage reject:', backendErr);
          throw backendErr;
        }
        // 注意：此处不检查 running——消息已收到、后端已算完，回复必须写出，
        // 否则对账/重启恰好打断时会吞掉这条回复（用户发了没回应）
        console.log(`[ai-chat][bg] 后端返回 ${response.length} 字符，写入会话 convId=${event.convId}`);

        // 写入 bot 回复到会话（显示为 bot 的消息）
        const replyId = generateId('br');
        await messagesApi!.sendResponse(
          event.convId,                     // 主聊天窗口的会话 ID
          rootId,                           // bot 的 rootId（作为 senderId）
          bot.name,                         // 发送者显示名
          replyId,                          // 新消息 ID
          response,                         // AI 回复文本
        );
        console.log(`[ai-chat][bg] 回复已写入会话`);
      } catch (err) {
        console.error('[ai-chat] Bot 消息处理异常:', err);
        await new Promise((r) => setTimeout(r, 2000));
      }
    }
  }

  poll();

  return { stop: () => { running = false; } };
}

// ------------------------------------------------------------------
// Bot 联系人激活编排（注册 + 监听，模块级注册表防泄漏）
// ------------------------------------------------------------------

/** 活跃 bot 消息处理器注册表（按 botId 索引；模块级单例，视图重挂载不会重复启动） */
const activeProcessors = new Map<string, StopToken>();

/**
 * 激活 bot 联系人：注册/刷新通讯录名片 + 启动主聊天消息监听。
 * 幂等：重复激活同一 bot 会先停掉旧监听器再重新注册——编辑改名后调用
 * 可刷新通讯录显示名（内核 contact_ensure_bot 对已存在联系人更新 nickname）。
 */
export async function activateBotContact(sdk: PluginSDK, bot: BotInstance): Promise<void> {
  deactivateBotContact(bot.id);
  if (!sdk.messages) {
    console.warn('[ai-chat] messages 模块不可用，跳过联系人激活');
    return;
  }
  await registerBotContact(sdk.messages, bot.id, bot.name);
  activeProcessors.set(bot.id, startBotMessageProcessor(sdk, bot));
}

/** 停用指定 bot 的消息监听（删除 bot 时调用） */
export function deactivateBotContact(botId: string): void {
  activeProcessors.get(botId)?.stop();
  activeProcessors.delete(botId);
}

/**
 * 仅注册/刷新联系人名片，不启动消息监听。
 * 主视图（app）在新建/编辑 bot 时调用——让联系人立即出现/刷新在通讯录。
 * 消息监听统一由后台视图（background）承载，主视图不启动监听，
 * 避免主视图与后台视图两个独立 iframe 上下文重复消费消息导致重复回复。
 */
export async function registerBotContactOnly(sdk: PluginSDK, bot: BotInstance): Promise<void> {
  if (!sdk.messages) return;
  await registerBotContact(sdk.messages, bot.id, bot.name);
}

/** 停用全部 bot 消息监听（视图卸载时调用，防止重复轮询/重复回复） */
export function deactivateAllBotContacts(): void {
  for (const token of activeProcessors.values()) token.stop();
  activeProcessors.clear();
}

/**
 * 后台常驻入口（background 视图）：启动全部 bot 的联系人注册 + 消息监听。
 * 后台视图随插件启用即启动、随应用存活（隐藏 iframe，无 UI），
 * 因此 bot 联系人监听不再依赖主聊天界面是否打开——bot 常驻在线。
 * 幂等：重复调用不会重复启动（activateBotContact 内部先停旧监听器）。
 */
export async function startBackgroundBotListeners(sdk: PluginSDK): Promise<void> {
  // 注册宿主反向查询处理器：宿主删除 bot 联系人前询问「该 bot 是否还存在」，
  // 据此决定放行（孤儿/已删）或拦截（仍在，引导去插件删）。
  sdk.onHostCall?.('bot:query', async (payload) => {
    const contactId = (payload as { contactId?: string })?.contactId ?? '';
    // contactId 形如 bot:ai-chat:{botId}，提取 botId 查是否仍在插件 bot 列表
    const botId = contactId.startsWith('bot:') ? contactId.split(':').slice(2).join(':') : '';
    if (!botId) return { exists: false };
    const bots = await listBots(sdk.docs);
    return { exists: bots.some((b) => b.id === botId) };
  });

  // 每个 bot 的配置签名：仅在 bot 增删或配置变化时才重启监听。
  // 关键：不能每次对账都无脑先停后启——CLI 调用要数秒，若对账撞上正在进行的
  // 后端调用，deactivateBotContact 会把 poll 循环的 running 置 false，导致
  // 「收到消息→后端返回→但 running 已 false→跳过 sendResponse」的吞消息 bug。
  const signatures = new Map<string, string>();
  const signatureOf = (bot: BotInstance): string =>
    JSON.stringify([bot.name, bot.backendType, bot.backendConfig, bot.systemPrompt]);

  /** 对账一次：新增/变更的 bot 启动或重启监听，删除的 bot 停掉监听 */
  const reconcile = async (): Promise<void> => {
    try {
      const bots = await listBots(sdk.docs);
      const aliveIds = new Set(bots.map((b) => b.id));
      // 停掉已删除 bot 的监听
      for (const activeId of Array.from(activeProcessors.keys())) {
        if (!aliveIds.has(activeId)) {
          signatures.delete(activeId);
          deactivateBotContact(activeId);
        }
      }
      // 仅新增或配置变化的 bot 才重启监听（未变的保持运行，不打断进行中的调用）
      for (const bot of bots) {
        const sig = signatureOf(bot);
        if (signatures.get(bot.id) === sig) continue; // 无变化，跳过
        signatures.set(bot.id, sig);
        try {
          await activateBotContact(sdk, bot);
        } catch (err) {
          console.error(`[ai-chat][background] 激活 bot「${bot.name}」失败:`, err);
        }
      }
    } catch (err) {
      console.error('[ai-chat][background] 加载 bot 列表失败:', err);
    }
  };

  await reconcile();
  // 周期对账：主视图增删改 bot 后，后台视图在此感知并同步监听
  setInterval(() => void reconcile(), 10_000);
}

/**
 * 处理从主聊天窗口发来的 bot 消息。
 * 仅传递最后一条用户消息给后端（主聊天窗口无 Plugin 聊天历史上下文）。
 */
async function handleMainChatMessage(
  bot: BotInstance,
  text: string,
): Promise<string> {
  const now = Date.now();
  const messages: ChatMessageRecord[] = [];

  if (bot.systemPrompt) {
    messages.push({
      id: 'sys-main',
      botInstanceId: bot.id,
      role: 'system',
      content: bot.systemPrompt,
      createdAt: 0,
    });
  }

  messages.push({
    id: `usr-${now}`,
    botInstanceId: bot.id,
    role: 'user',
    content: text,
    createdAt: now,
  });

  console.log(`[ai-chat][bg] bot.backendConfig=${JSON.stringify(bot.backendConfig)}`);
  const result = await callBackend(bot, messages);
  console.log(`[ai-chat][bg] callBackend 返回 text长度=${result.text?.length ?? 0} error=${result.error ?? '(无)'}`);
  return result.text || result.error || '（无响应）';
}
