/**
 * ai-chat 插件 · 数据模型与纯函数。
 *
 * 设计要点：
 * - 本文件不依赖 SDK / Vue，全是类型定义和可单测的纯函数；
 * - Bot 实例的 backend_config 为插件自定义的不透明 JSON，内核不感知；
 * - 消息角色区分 user/assistant/system，为多轮对话上下文做准备；
 * - 消息摘要生成遵循服务号模型的 summary 强制要求。
 */

// ------------------------------------------------------------------
// 集合名常量
// ------------------------------------------------------------------

/** Bot 实例配置（lww 策略：用户可自由更新配置） */
export const BOTS_COLLECTION = 'ai_chat_bots' as const;

/** 聊天历史消息（lww 策略：按 bot 实例分桶，query 按 botInstanceId 过滤） */
export const CHAT_HISTORY_COLLECTION = 'ai_chat_messages' as const;

// ------------------------------------------------------------------
// 后端提供者类型
// ------------------------------------------------------------------

/** 后端类型标识：由插件自行定义，内核不感知 */
export type BackendType = 'codebuddy' | 'openai' | 'ollama';

/** 后端调用上下文：传递给 BackendProvider.call 的完整参数 */
export type BackendCallContext = {
  /** Bot 实例的后端配置（插件定义的 JSON） */
  config: Record<string, unknown>;
  /** 完整的消息历史（包含当前用户消息），用于多轮对话上下文 */
  messages: ChatMessageRecord[];
};

/** 后端调用结果 */
export type BackendCallResult = {
  /** AI 回复文本（Markdown 格式） */
  text: string;
  /** 调用耗时（毫秒） */
  durationMs: number;
  /** 错误信息（调用成功时为 null） */
  error?: string;
};

/**
 * 本地 CLI 类后端的环境依赖声明。
 * 仅命令行类后端（codebuddy 等）需要；纯 HTTP 后端（openai）无此字段。
 */
export type BackendEnvironment = {
  /**
   * 候选可执行命令名列表（按优先级）。
   * 探测时逐个在 PATH 中查找，全部命中则让用户选择，单个命中直接用，
   * 全部未命中则引导"手动填路径"或"一键安装"。
   * 例如 codebuddy 后端：['codebuddy', 'codebuddy-code', 'cbc']。
   */
  candidateCommands: string[];
  /** 探测是否可用的子命令参数（如 ['--version']），执行成功即视为就绪 */
  probeArgs: string[];
  /** 自动安装命令（npm 全局安装），如 ['install','-g','@tencent-ai/codebuddy-code'] */
  installArgs: string[];
  /** 安装命令的可执行程序（通常 'npm'） */
  installProgram: string;
  /** 给用户看的手动安装提示（一键安装失败时回退展示） */
  manualHint: string;
  /** 一键安装的产物命令名（安装成功后用于复检），通常是候选中的第一个 */
  installedCommand: string;
};

/**
 * 后端提供者接口：每种后端类型对应一个实现。
 * 当内核暴露 sys.exec / sys.fetch 后，各实现替换为真实调用。
 */
export type BackendProvider = {
  /** 后端类型标识 */
  type: BackendType;
  /** 执行调用 */
  call: (ctx: BackendCallContext) => Promise<BackendCallResult>;
  /** 本地环境依赖声明（仅命令行类后端提供，用于环境检测与一键安装） */
  env?: BackendEnvironment;
};

// ------------------------------------------------------------------
// ChatMessage 角色
// ------------------------------------------------------------------

export type ChatRole = 'system' | 'user' | 'assistant';

// ------------------------------------------------------------------
// Bot 实例
// ------------------------------------------------------------------

/** Bot 实例文档（存储于 docs.{BOTS_COLLECTION}） */
export type BotInstance = {
  /** doc id = 用户指定的唯一标识，如 'bot-001' */
  id: string;
  /** 显示名称 */
  name: string;
  /** 头像 URL（可选，未设置时使用默认图标） */
  avatarUrl?: string;
  /** 后端类型 */
  backendType: BackendType;
  /** 后端配置（不透明 JSON，插件自行解析） */
  backendConfig: Record<string, unknown>;
  /** 系统提示词（可选） */
  systemPrompt?: string;
  /** 创建时间（Unix 毫秒） */
  createdAt: number;
};

// ------------------------------------------------------------------
// 聊天消息
// ------------------------------------------------------------------

/** 聊天消息文档（存储于 docs.{CHAT_HISTORY_COLLECTION}） */
export type ChatMessageRecord = {
  /** doc id：自动生成 */
  id: string;
  /** 所属 Bot 实例 id */
  botInstanceId: string;
  /** 消息角色 */
  role: ChatRole;
  /** 消息内容（Markdown 格式） */
  content: string;
  /** 发送时间（Unix 毫秒） */
  createdAt: number;
  /** 调用耗时（仅 assistant 消息；单位毫秒） */
  durationMs?: number;
  /** 错误信息（仅 assistant 消息且调用失败时存在） */
  error?: string;
};

// ------------------------------------------------------------------
// 消息摘要
// ------------------------------------------------------------------

/** 应用消息 summary 的最大字符数（对齐内核约束：trim 后 ≤200） */
export const MAX_SUMMARY_LENGTH = 200;

/** 摘要中内容预览最大字数 */
export const SUMMARY_PREVIEW_LENGTH = 50;

/**
 * 构建应用消息摘要。
 * 未安装插件的设备上壳层原生渲染此纯文本，必须自成一体。
 */
export function buildMessageSummary(
  role: ChatRole,
  content: string,
): string {
  const trimmed = content.trim();
  const preview =
    trimmed.length > SUMMARY_PREVIEW_LENGTH
      ? `${trimmed.slice(0, SUMMARY_PREVIEW_LENGTH)}…`
      : trimmed;

  if (role === 'user') {
    return `【用户】${preview}`;
  }
  if (role === 'assistant') {
    return `【AI】${preview}`;
  }
  return preview;
}

// ------------------------------------------------------------------
// 轻量级 Markdown → HTML 渲染器
// ------------------------------------------------------------------

/**
 * 将 AI 返回的 Markdown 文本转为 HTML。
 *
 * 为什么自建而非引入第三方库：
 * - 插件 bundle 在沙箱 iframe 中，应尽量精简体积；
 * - AI 输出通常只涉及代码块、粗体、斜体、列表、段落，不需完整 CommonMark；
 * - 自建渲染器可控，不受上游库安全漏洞影响（innerHTML 已信任来源是 AI 后端）。
 *
 * 安全假设：本函数输出的 HTML 经插件自身生成后用于 innerHTML，信任链为
 * 后端 → 插件 → 渲染器。不处理用户输入的未过滤 Markdown。
 */
export function renderMarkdown(raw: string): string {
  if (!raw) {
    return '';
  }

  // 转义 HTML 实体（防止 Markdown 中混入的 HTML 标签被执行）
  const escapeHtml = (s: string): string =>
    s
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');

  // 先分割代码块，分别处理
  const lines = raw.split('\n');
  const result: string[] = [];
  let inCodeBlock = false;
  let codeLang = '';
  let codeBuffer: string[] = [];

  const flushCodeBlock = (): string => {
    if (codeBuffer.length === 0) {
      return '';
    }
    const code = codeBuffer.join('\n');
    const langAttr = codeLang ? ` class="language-${escapeHtml(codeLang)}"` : '';
    codeBuffer = [];
    codeLang = '';
    return `<pre><code${langAttr}>${code}</code></pre>`;
  };

  const renderInline = (text: string): string => {
    let html = escapeHtml(text);
    // 粗体 **text**
    html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
    // 斜体 *text*
    html = html.replace(/\*(.+?)\*/g, '<em>$1</em>');
    // 行内代码 `code`
    html = html.replace(/`(.+?)`/g, '<code>$1</code>');
    return html;
  };

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];

    // 代码块开始/结束
    if (line.trimStart().startsWith('```')) {
      if (!inCodeBlock) {
        // flush pending paragraph before code block
        if (result.length > 0 && result[result.length - 1] !== '') {
          result.push('');
        }
        inCodeBlock = true;
        codeLang = line.trimStart().slice(3).trim();
        continue;
      } else {
        inCodeBlock = false;
        result.push(flushCodeBlock());
        continue;
      }
    }

    if (inCodeBlock) {
      codeBuffer.push(line);
      continue;
    }

    // 空行：段落分隔
    if (line.trim() === '') {
      if (result.length > 0 && result[result.length - 1] !== '') {
        result.push('');
      }
      continue;
    }

    // 标题
    const headingMatch = line.match(/^(#{1,6})\s+(.+)/);
    if (headingMatch) {
      const level = headingMatch[1].length;
      const text = renderInline(headingMatch[2].trim());
      result.push(`<h${level}>${text}</h${level}>`);
      continue;
    }

    // 无序列表
    const ulMatch = line.match(/^(\s*)[-*+]\s+(.+)/);
    if (ulMatch) {
      const indent = Math.floor(ulMatch[1].length / 2);
      const text = renderInline(ulMatch[2].trim());
      const padding = indent > 0 ? ` style="margin-left:${indent * 1.5}em"` : '';
      result.push(`<li${padding}>${text}</li>`);
      // 如果下一行不是列表项，闭合 ul
      const nextLine = i + 1 < lines.length ? lines[i + 1] : '';
      if (!nextLine.match(/^(\s*)[-*+]\s+/)) {
        // Wrap preceding <li> elements in <ul>
        wrapListItems(result);
      }
      continue;
    }

    // 有序列表
    const olMatch = line.match(/^(\s*)\d+\.\s+(.+)/);
    if (olMatch) {
      const indent = Math.floor(olMatch[1].length / 2);
      const text = renderInline(olMatch[2].trim());
      const padding = indent > 0 ? ` style="margin-left:${indent * 1.5}em"` : '';
      result.push(`<li${padding}>${text}</li>`);
      const nextLine = i + 1 < lines.length ? lines[i + 1] : '';
      if (!nextLine.match(/^(\s*)\d+\.\s+/)) {
        wrapOrderedListItems(result);
      }
      continue;
    }

    // 引用
    if (line.startsWith('>')) {
      const text = renderInline(line.slice(1).trim());
      result.push(`<blockquote>${text}</blockquote>`);
      continue;
    }

    // 水平线
    if (line.trim().match(/^(-{3,}|_{3,}|\*{3,})$/)) {
      result.push('<hr/>');
      continue;
    }

    // 普通段落
    result.push(`<p>${renderInline(line)}</p>`);
  }

  // 末尾未闭合的代码块
  if (inCodeBlock) {
    result.push(flushCodeBlock());
  }

  // 末尾未闭合的列表
  wrapListItems(result);
  wrapOrderedListItems(result);

  return result.filter((s) => s !== '').join('\n');
}

/** 将 result 中连续的 <li> 用 <ul> 包裹 */
function wrapListItems(result: string[]): void {
  let i = result.length - 1;
  while (i >= 0 && result[i].startsWith('<li') && !result[i].startsWith('<li style')) {
    i -= 1;
  }
  // Also include nested <li> with style
  while (i >= 0 && (result[i]?.startsWith('<li') ?? false)) {
    i -= 1;
  }
  const start = i + 1;
  const items = result.slice(start);
  const nonLi = items.findIndex((s) => !s.startsWith('<li'));
  if (nonLi === -1 && items.length > 0) {
    result.splice(start, items.length, `<ul>${items.join('')}</ul>`);
  } else if (nonLi > 0) {
    const liItems = items.slice(0, nonLi);
    const rest = items.slice(nonLi);
    result.splice(start, items.length, `<ul>${liItems.join('')}</ul>`, ...rest);
  }
}

function wrapOrderedListItems(result: string[]): void {
  let i = result.length - 1;
  while (i >= 0 && result[i].startsWith('<li') && !result[i].startsWith('<li style')) {
    i -= 1;
  }
  while (i >= 0 && (result[i]?.startsWith('<li') ?? false)) {
    i -= 1;
  }
  const start = i + 1;
  const items = result.slice(start);
  const nonLi = items.findIndex((s) => !s.startsWith('<li'));
  if (nonLi === -1 && items.length > 0) {
    result.splice(start, items.length, `<ol>${items.join('')}</ol>`);
  } else if (nonLi > 0) {
    const liItems = items.slice(0, nonLi);
    const rest = items.slice(nonLi);
    result.splice(start, items.length, `<ol>${liItems.join('')}</ol>`, ...rest);
  }
}

// ------------------------------------------------------------------
// 消息内容校验
// ------------------------------------------------------------------

/** 用户消息最大长度 */
export const MAX_MESSAGE_LENGTH = 4000;

export type ValidationResult =
  | { ok: true }
  | { ok: false; reason: string };

export function validateMessage(content: string): ValidationResult {
  const trimmed = content.trim();
  if (!trimmed) {
    return { ok: false, reason: '消息不能为空' };
  }
  if (trimmed.length > MAX_MESSAGE_LENGTH) {
    return {
      ok: false,
      reason: `消息长度不能超过 ${MAX_MESSAGE_LENGTH} 字`,
    };
  }
  return { ok: true };
}
