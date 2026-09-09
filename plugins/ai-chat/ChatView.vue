<template>
  <!-- 初始化加载态：UI 已挂载（先于桥握手），等待连接宿主与数据加载完成 -->
  <div v-if="initState === 'loading'" class="ai-chat-root init-state">
    <div class="init-placeholder">
      <div class="init-spinner"></div>
      <p class="init-text">{{ initHint }}</p>
    </div>
  </div>

  <!-- 初始化失败态 -->
  <div v-else-if="initState === 'failed'" class="ai-chat-root init-state">
    <div class="init-placeholder">
      <p class="init-text init-text--error">AI 聊天插件初始化失败</p>
      <p class="init-detail">{{ initError }}</p>
    </div>
  </div>

  <!-- 正常态 -->
  <div v-else class="ai-chat-root" :class="{ dark: isDark }">
    <!-- 左侧：Bot 列表（宽窗常驻两栏；窄窗折叠为覆盖抽屉，顶栏 ☰ 打开） -->
    <aside
      class="sidebar"
      :class="{ 'sidebar--overlay': isNarrowLayout, 'sidebar--open': sidebarOpen }"
      @click.self="closeSidebar"
    >
      <div class="sidebar-header">
        <h2 class="sidebar-title">AI 聊天</h2>
        <div class="sidebar-header-actions">
          <button
            v-if="isNarrowLayout"
            class="btn-sidebar-close"
            @click="closeSidebar"
            title="收起列表"
          >
            &times;
          </button>
          <button class="btn-add-bot" @click="openBotForm()" title="新建 Bot">
            <span class="btn-add-icon">+</span>
          </button>
        </div>
      </div>
      <div class="bot-list" v-if="bots.length > 0">
        <div
          v-for="bot in bots"
          :key="bot.id"
          class="bot-item"
          :class="{ active: activeBotId === bot.id }"
          @click="selectBot(bot.id)"
        >
          <div class="bot-avatar">
            <img v-if="bot.avatarUrl" :src="bot.avatarUrl" :alt="bot.name" />
            <span v-else class="bot-avatar-default">{{ bot.name.charAt(0) }}</span>
          </div>
          <div class="bot-info">
            <div class="bot-name">{{ bot.name }}</div>
            <div class="bot-type">{{ backendLabel(bot.backendType) }}</div>
          </div>
          <button
            class="bot-menu-btn"
            @click.stop="openBotForm(bot)"
            title="编辑"
          >
            &#8230;
          </button>
        </div>
      </div>
      <div v-else class="sidebar-empty">
        <p>还没有 Bot 实例</p>
        <p class="sidebar-empty-hint">点击右上角 + 创建第一个</p>
      </div>
    </aside>

    <!-- 窄窗抽屉遮罩：点击空白处收起侧栏 -->
    <div
      v-if="isNarrowLayout && sidebarOpen"
      class="sidebar-backdrop"
      @click="closeSidebar"
    ></div>

    <!-- 右侧：聊天区 -->
    <main class="chat-area" v-if="activeBotId">
      <!-- 聊天头部 -->
      <header class="chat-header">
        <button
          v-if="isNarrowLayout"
          class="btn-sidebar-toggle"
          @click="openSidebar"
          title="Bot 列表"
          aria-label="打开 Bot 列表"
        >
          ☰
        </button>
        <div class="chat-header-info">
          <strong>{{ activeBot?.name ?? '...' }}</strong>
          <span class="chat-header-type">{{ backendLabel(activeBot?.backendType) }}</span>
        </div>
      </header>

      <!-- 消息列表 -->
      <div class="message-list" ref="messageListRef">
        <div v-if="loadingMessages" class="messages-loading">加载中…</div>
        <template v-for="msg in messages" :key="msg.id">
          <!-- 用户消息：保持气泡样式 -->
          <div v-if="msg.role === 'user'" class="message-row message-row--user">
            <div class="message-bubble message-bubble--user">
              <div class="message-text">{{ msg.content }}</div>
              <div v-if="msg.error" class="message-error">{{ msg.error }}</div>
              <div class="message-meta">
                <span>{{ formatTime(msg.createdAt) }}</span>
              </div>
            </div>
          </div>
          <!-- AI 消息：无气泡，纯文本块（ChatGPT/Claude 风格） -->
          <div v-else class="message-row message-row--bot">
            <div class="bot-message-block">
              <div class="bot-msg-avatar">
                <img v-if="activeBot?.avatarUrl" :src="activeBot.avatarUrl" :alt="activeBot?.name" />
                <span v-else class="bot-msg-avatar-default">{{ activeBot?.name?.charAt(0) }}</span>
              </div>
              <div class="bot-msg-body">
                <div class="bot-msg-name">{{ activeBot?.name }}</div>
                <!-- 流式输出中：纯文本 + 闪烁光标 -->
                <div v-if="msg.streaming" class="bot-msg-text streaming">
                  {{ msg.content }}<span class="streaming-cursor">█</span>
                </div>
                <!-- 完成：Markdown 渲染 -->
                <div v-else class="bot-msg-content">
                  <div class="message-html" v-html="renderMarkdown(msg.content)"></div>
                  <div v-if="msg.error" class="message-error">{{ msg.error }}</div>
                  <div class="message-meta">
                    <span>{{ formatTime(msg.createdAt) }}</span>
                    <span v-if="msg.durationMs"> · {{ (msg.durationMs / 1000).toFixed(1) }}s</span>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </template>
        <div v-if="messages.length === 0 && !loadingMessages" class="messages-empty">
          向 {{ activeBot?.name ?? 'Bot' }} 发送第一条消息吧
        </div>
      </div>

      <!-- 输入区域 -->
      <footer class="chat-input-area">
        <textarea
          class="chat-input"
          v-model="inputText"
          :placeholder="`向 ${activeBot?.name ?? 'Bot'} 输入消息…`"
          :disabled="thinking"
          @keydown.enter.exact="handleSend"
          rows="2"
          ref="inputRef"
        ></textarea>
        <button
          class="btn-send"
          :disabled="!inputText.trim() || thinking"
          @click="handleSend"
        >
          发送
        </button>
      </footer>
    </main>

    <!-- 未选择 Bot -->
    <main v-else class="chat-area chat-area--empty">
      <button
        v-if="isNarrowLayout"
        class="btn-sidebar-toggle btn-sidebar-toggle--empty"
        @click="openSidebar"
        title="Bot 列表"
        aria-label="打开 Bot 列表"
      >
        ☰
      </button>
      <div class="no-bot-selected">
        <p class="no-bot-icon">🤖</p>
        <p>选择一个 Bot 开始聊天</p>
      </div>
    </main>

    <!-- Bot 创建/编辑弹窗 -->
    <dialog ref="botFormDialogRef" class="bot-form-dialog" @close="closeBotForm">
      <div class="dialog-content" v-if="botForm.visible" @click.stop>
        <h3>{{ botForm.editing ? '编辑 Bot' : '新建 Bot' }}</h3>

        <!-- 名称 -->
        <label class="form-field">
          <span class="form-label">名称</span>
          <input
            class="form-input"
            v-model="botForm.name"
            placeholder="如：CodeBuddy-工作"
            maxlength="50"
          />
        </label>

        <!-- 后端类型 -->
        <label class="form-field">
          <span class="form-label">后端类型</span>
          <select class="form-input" v-model="botForm.backendType">
            <option value="codebuddy">CodeBuddy CLI</option>
            <option value="openai">OpenAI 兼容 API</option>
            <option value="ollama">Ollama</option>
          </select>
        </label>

        <!-- CodeBuddy 配置 -->
        <template v-if="botForm.backendType === 'codebuddy'">
          <label class="form-field">
            <span class="form-label">CLI 路径</span>
            <input
              class="form-input"
              v-model="botForm.config.codebuddy.cliPath"
              placeholder="codebuddy（默认）"
            />
          </label>
          <label class="form-field">
            <span class="form-label">工作目录</span>
            <div class="model-select-row">
              <input
                class="form-input"
                v-model="botForm.config.codebuddy.workdir"
                placeholder="点右侧 📁 选择目录（或手动输入路径）"
              />
              <button
                class="btn-model-refresh"
                title="打开系统目录选择对话框"
                :disabled="workdirPicking"
                @click="pickWorkdir"
              >{{ workdirPicking ? '…' : '📁' }}</button>
            </div>
          </label>
          <label class="form-field">
            <span class="form-label">模型</span>
            <div class="model-select-row">
              <input
                class="form-input"
                v-model="botForm.config.codebuddy.model"
                placeholder="默认（CLI 内置默认模型）"
                list="codebuddy-model-list"
              />
              <datalist id="codebuddy-model-list">
                <option v-for="m in codebuddyModels" :key="m" :value="m" />
              </datalist>
              <button
                class="btn-model-refresh"
                title="获取 CodeBuddy 支持的模型列表（解析 codebuddy --help）"
                :disabled="modelsLoading.codebuddy"
                @click="fetchCodebuddyModels"
              >{{ modelsLoading.codebuddy ? '…' : '↻' }}</button>
            </div>
          </label>
          <p class="form-hint">
            执行 <code>codebuddy --print -- "..."</code>（<code>--</code> 终止选项解析，消息以 <code>-</code> 开头也不会被当成 CLI 标志），stdout 即为回复。模型从 <code>codebuddy --help</code> 的 <code>--model</code> 枚举解析（点 ↻ 刷新），留空用 CLI 内置默认模型，也可手填。工作目录是 CLI 读取代码/文档上下文的根路径，留空则继承宿主进程目录（不可控，不建议）。
          </p>

          <!-- 环境检测与一键安装 -->
          <div v-if="envCheck.state !== 'idle'" class="env-check">
            <!-- 扫描中 -->
            <p v-if="envCheck.state === 'scanning'" class="env-check-line env-check-line--info">
              正在检测本地 CodeBuddy 环境…
            </p>

            <!-- 已检测到（单/多候选） -->
            <template v-else-if="envCheck.state === 'found'">
              <p class="env-check-line env-check-line--ok">
                ✓ 检测到 {{ envCheck.candidates.length }} 个可用的 CodeBuddy CLI
              </p>
              <!-- 多候选时让用户选择 -->
              <div v-if="envCheck.candidates.length > 1" class="env-candidates">
                <label
                  v-for="c in envCheck.candidates"
                  :key="c.command"
                  class="env-candidate"
                >
                  <input
                    type="radio"
                    name="env-candidate"
                    :value="c.command"
                    :checked="botForm.config.codebuddy.cliPath === c.command"
                    @change="selectCandidate(c.command)"
                  />
                  <span class="env-candidate-name">{{ c.command }}</span>
                  <span class="env-candidate-src">{{ c.source === 'manual' ? '手动指定' : 'PATH' }}</span>
                </label>
              </div>
              <!-- 单候选直接展示 -->
              <p v-else class="form-hint">
                将使用命令：<code>{{ envCheck.candidates[0].command }}</code>
              </p>
            </template>

            <!-- 未检测到：手动填路径 或 一键安装 -->
            <template v-else-if="envCheck.state === 'missing'">
              <p class="env-check-line env-check-line--warn">
                ⚠ 未在 PATH 中检测到 CodeBuddy CLI
              </p>

              <!-- 方案一：手动指定路径 -->
              <div class="env-manual">
                <input
                  class="form-input"
                  v-model="envCheck.manualInput"
                  placeholder="手动填写可执行文件路径或命令名"
                  @keyup.enter="handleUseManualPath"
                />
                <button type="button" class="btn btn--small" @click="handleUseManualPath">
                  验证并使用
                </button>
              </div>
              <p v-if="envCheck.manualError" class="env-check-line env-check-line--error">
                {{ envCheck.manualError }}
              </p>

              <!-- 方案二：一键安装 -->
              <div class="env-install">
                <span class="env-install-or">或</span>
                <button type="button" class="btn btn--small" @click="handleInstallEnv">
                  一键安装 CLI 版本
                </button>
              </div>
              <p class="form-hint">
                将执行：<code>{{ envCheck.manualHint }}</code>
              </p>
              <pre v-if="envCheck.installFailed && envCheck.installOutput" class="env-check-output">{{ envCheck.installOutput }}</pre>
            </template>

            <!-- 安装中 -->
            <p v-else-if="envCheck.state === 'installing'" class="env-check-line env-check-line--info">
              正在安装 CodeBuddy CLI（可能需要 1~2 分钟）…
            </p>
          </div>
        </template>

        <!-- OpenAI 配置 -->
        <template v-if="botForm.backendType === 'openai'">
          <label class="form-field">
            <span class="form-label">Base URL</span>
            <input
              class="form-input"
              v-model="botForm.config.openai.baseUrl"
              placeholder="https://api.openai.com/v1"
            />
          </label>
          <label class="form-field">
            <span class="form-label">API Key</span>
            <input
              class="form-input"
              type="password"
              v-model="botForm.config.openai.apiKey"
              placeholder="sk-xxx"
            />
          </label>
          <label class="form-field">
            <span class="form-label">模型</span>
            <div class="model-select-row">
              <input
                class="form-input"
                v-model="botForm.config.openai.model"
                placeholder="gpt-4o"
                list="openai-model-list"
              />
              <datalist id="openai-model-list">
                <option v-for="m in openaiModels" :key="m" :value="m" />
              </datalist>
              <button
                class="btn-model-refresh"
                title="获取可用模型列表"
                :disabled="modelsLoading.openai"
                @click="fetchOpenaiModels"
              >{{ modelsLoading.openai ? '…' : '↻' }}</button>
            </div>
            <p class="form-hint">
              可从下拉列表选择，也可自由输入。点击 ↻ 从 API 拉取可用模型。
            </p>
          </label>
        </template>

        <!-- Ollama 配置 -->
        <template v-if="botForm.backendType === 'ollama'">
          <label class="form-field">
            <span class="form-label">端点</span>
            <input
              class="form-input"
              v-model="botForm.config.ollama.endpoint"
              placeholder="http://localhost:11434"
            />
          </label>
          <label class="form-field">
            <span class="form-label">模型</span>
            <div class="model-select-row">
              <input
                class="form-input"
                v-model="botForm.config.ollama.model"
                placeholder="qwen2.5:7b"
                list="ollama-model-list"
              />
              <datalist id="ollama-model-list">
                <option v-for="m in ollamaModels" :key="m" :value="m" />
              </datalist>
              <button
                class="btn-model-refresh"
                title="获取可用模型列表"
                :disabled="modelsLoading.ollama"
                @click="fetchOllamaModels"
              >{{ modelsLoading.ollama ? '…' : '↻' }}</button>
            </div>
            <p class="form-hint">
              可从下拉列表选择，也可自由输入。点击 ↻ 从 Ollama 拉取已安装模型。
            </p>
          </label>
        </template>

        <!-- 系统提示词（通用） -->
        <label class="form-field">
          <span class="form-label">系统提示词（可选）</span>
          <textarea
            class="form-input form-textarea"
            v-model="botForm.systemPrompt"
            placeholder="你是一个有用的助手…"
            rows="3"
          ></textarea>
        </label>

        <!-- 按钮行 -->
        <div class="dialog-actions">
          <button
            v-if="botForm.editing"
            class="btn-danger"
            @click="handleDeleteBot"
          >
            删除
          </button>
          <div class="dialog-actions-right">
            <button class="btn-cancel" @click="closeBotForm">取消</button>
            <button class="btn-confirm" :disabled="!botForm.name.trim()" @click="handleSaveBot">
              {{ botForm.editing ? '保存' : '创建' }}
            </button>
          </div>
        </div>
      </div>
    </dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, nextTick, onMounted, watch } from 'vue';
import { ensurePluginSDK, type PluginSDK } from '../../packages/plugin-sdk/src/index';
import {
  listBots,
  createBot,
  updateBot,
  deleteBot,
  listChatMessages,
  processChat,
  registerBotContactOnly,
  generateId,
  botRootId,
  scanBackendEnv,
  verifyManualCommand,
  installBackendEnv,
  getBackendProvider,
  type EnvCandidate,
} from './service';
import type { BackendType, BotInstance, ChatMessageRecord } from './model';
import { renderMarkdown, validateMessage } from './model';
import { isNarrowLayout } from './ui-layout';

// ------------------------------------------------------------------
// 状态
// ------------------------------------------------------------------

let sdk: PluginSDK | null = null;
const isDark = ref(false);
const bots = ref<BotInstance[]>([]);

/** SDK 握手/初始化状态：loading=等待桥握手，ready=就绪，failed=握手失败 */
const initState = ref<'loading' | 'ready' | 'failed'>('loading');
/** 握手失败原因（展示给用户） */
const initError = ref('');
/** 加载阶段提示（loading 页文案：连接宿主→加载数据 分阶段提示） */
const initHint = ref('正在连接宿主环境…');
const activeBotId = ref<string | null>(null);
const activeBot = ref<BotInstance | null>(null);
const messages = ref<ChatMessageRecord[]>([]);
const inputText = ref('');
const thinking = ref(false);
const loadingMessages = ref(false);

/** 窄窗抽屉开态（宽窗下无意义：侧栏常驻，--overlay 类不生效） */
const sidebarOpen = ref(false);

function openSidebar(): void {
  sidebarOpen.value = true;
}

function closeSidebar(): void {
  sidebarOpen.value = false;
}

// 宽窄模式切换时复位抽屉：切宽窗抽屉状态无意义，切窄窗默认收起
watch(isNarrowLayout, () => {
  sidebarOpen.value = false;
});

/** OpenAI / Ollama / CodeBuddy 可用模型列表（datalist 候选） */
const openaiModels = ref<string[]>([]);
const ollamaModels = ref<string[]>([]);
const codebuddyModels = ref<string[]>([]);
const modelsLoading = ref<Record<string, boolean>>({ openai: false, ollama: false, codebuddy: false });

/** 获取 OpenAI 兼容 API 的可用模型列表（GET /models） */
async function fetchOpenaiModels(): Promise<void> {
  const provider = getBackendProvider('openai');
  if (!provider?.listModels) return;
  const cfg: Record<string, unknown> = {};
  if (botForm.config.openai.baseUrl) cfg.baseUrl = botForm.config.openai.baseUrl;
  if (botForm.config.openai.apiKey) cfg.apiKey = botForm.config.openai.apiKey;
  if (!cfg.baseUrl) {
    openaiModels.value = [];
    return;
  }
  modelsLoading.value.openai = true;
  try {
    openaiModels.value = await provider.listModels(cfg);
  } catch {
    openaiModels.value = [];
  } finally {
    modelsLoading.value.openai = false;
  }
}

/** 获取 Ollama 的可用模型列表（GET /api/tags） */
async function fetchOllamaModels(): Promise<void> {
  const provider = getBackendProvider('ollama');
  if (!provider?.listModels) return;
  const cfg: Record<string, unknown> = {};
  if (botForm.config.ollama.endpoint) cfg.endpoint = botForm.config.ollama.endpoint;
  if (!cfg.endpoint) {
    ollamaModels.value = [];
    return;
  }
  modelsLoading.value.ollama = true;
  try {
    ollamaModels.value = await provider.listModels(cfg);
  } catch {
    ollamaModels.value = [];
  } finally {
    modelsLoading.value.ollama = false;
  }
}

/** 工作目录选择进行中（防重复点击） */
const workdirPicking = ref(false);

/**
 * 打开系统目录选择对话框选工作目录（sys.pickFolder）。图形化选择替代手动输入，
 * 选中即回填表单；取消/失败（对话框不可用）时静默——输入框仍可手动编辑。
 */
async function pickWorkdir(): Promise<void> {
  if (!sdk?.sys?.pickFolder || workdirPicking.value) return;
  workdirPicking.value = true;
  try {
    const dir = await sdk.sys.pickFolder('选择 CodeBuddy 工作目录');
    if (dir) botForm.config.codebuddy.workdir = dir;
  } catch {
    // 对话框不可用（如非桥上下文）：静默，用户可手动输入
  } finally {
    workdirPicking.value = false;
  }
}

/** 获取 CodeBuddy CLI 支持的模型列表（解析 `codebuddy --help` 的 --model 枚举） */
async function fetchCodebuddyModels(): Promise<void> {
  const provider = getBackendProvider('codebuddy');
  if (!provider?.listModels) return;
  const cfg: Record<string, unknown> = {};
  if (botForm.config.codebuddy.cliPath) cfg.cliPath = botForm.config.codebuddy.cliPath;
  modelsLoading.value.codebuddy = true;
  try {
    codebuddyModels.value = await provider.listModels(cfg);
  } catch {
    codebuddyModels.value = [];
  } finally {
    modelsLoading.value.codebuddy = false;
  }
}

/** 注册所有已有 bot 为联系人（主视图只注册名片；消息监听统一由后台视图承载，
 *  避免主视图与后台视图两个独立 iframe 上下文重复消费消息导致重复回复） */
async function syncBotContactsAndListeners(): Promise<void> {
  if (!sdk) return;
  for (const bot of bots.value) {
    try {
      await registerBotContactOnly(sdk, bot);
    } catch (err) {
      console.error(`[ai-chat] 注册 bot「${bot.name}」联系人失败:`, err);
    }
  }
}

const messageListRef = ref<HTMLElement | null>(null);
const inputRef = ref<HTMLTextAreaElement | null>(null);
const botFormDialogRef = ref<HTMLDialogElement | null>(null);

// Bot 表单状态
type BotFormState = {
  visible: boolean;
  editing: boolean;
  name: string;
  backendType: BackendType;
  systemPrompt: string;
  config: {
    codebuddy: { cliPath: string; workdir: string; model: string };
    openai: { baseUrl: string; apiKey: string; model: string };
    ollama: { endpoint: string; model: string };
  };
  editingId?: string;
};

const defaultBotForm = (): BotFormState => ({
  visible: false,
  editing: false,
  name: '',
  backendType: 'codebuddy',
  systemPrompt: '',
  config: {
    codebuddy: { cliPath: '', workdir: '', model: '' },
    openai: { baseUrl: '', apiKey: '', model: '' },
    ollama: { endpoint: 'http://localhost:11434', model: '' },
  },
});

const botForm = reactive<BotFormState>(defaultBotForm());

// ------------------------------------------------------------------
// 后端环境检测与一键安装
// ------------------------------------------------------------------

/** 环境检测状态机：idle→scanning→found/missing；installing 为一键安装进行中 */
type EnvCheckState = 'idle' | 'scanning' | 'found' | 'missing' | 'installing';
const envCheck = reactive<{
  state: EnvCheckState;
  /** 检测到的可用命令候选（found 态） */
  candidates: EnvCandidate[];
  manualHint: string;
  /** 手动填写的路径输入框值（missing 态） */
  manualInput: string;
  /** 手动路径校验失败提示 */
  manualError: string;
  installOutput: string;
  installFailed: boolean;
}>({
  state: 'idle',
  candidates: [],
  manualHint: '',
  manualInput: '',
  manualError: '',
  installOutput: '',
  installFailed: false,
});

/** 扫描当前选中后端的本地环境（逐个探测候选命令） */
async function checkBackendEnv(): Promise<void> {
  if (!sdk) return;
  envCheck.state = 'scanning';
  envCheck.installOutput = '';
  envCheck.installFailed = false;
  envCheck.manualError = '';
  const scan = await scanBackendEnv(sdk, botForm.backendType);
  if (scan.state === 'found') {
    envCheck.state = 'found';
    envCheck.candidates = scan.candidates;
    // 编辑态：若已有配置且该命令仍在候选中，保留用户原选择；否则采用第一个候选
    const existing = botForm.config.codebuddy.cliPath;
    const stillValid = existing && scan.candidates.some((c) => c.command === existing);
    if (!stillValid) {
      botForm.config.codebuddy.cliPath = scan.candidates[0].command;
    }
  } else if (scan.state === 'missing') {
    envCheck.state = 'missing';
    envCheck.candidates = [];
    envCheck.manualHint = scan.manualHint;
  } else {
    // unsupported：纯 HTTP 后端无本地依赖，不展示检测块
    envCheck.state = 'idle';
  }
}

/** 用户选择某个候选命令 */
function selectCandidate(cmd: string): void {
  botForm.config.codebuddy.cliPath = cmd;
}

/** 校验并使用用户手动填写的路径 */
async function handleUseManualPath(): Promise<void> {
  if (!sdk || !envCheck.manualInput.trim()) return;
  envCheck.manualError = '';
  const candidate = await verifyManualCommand(sdk, botForm.backendType, envCheck.manualInput);
  if (candidate) {
    botForm.config.codebuddy.cliPath = candidate.command;
    envCheck.state = 'found';
    envCheck.candidates = [candidate];
  } else {
    envCheck.manualError = '该路径不可用（执行 --version 失败），请确认是可执行文件或已加入 PATH 的命令';
  }
}

/** 一键安装当前选中后端的本地 CLI */
async function handleInstallEnv(): Promise<void> {
  if (!sdk) return;
  envCheck.state = 'installing';
  const result = await installBackendEnv(sdk, botForm.backendType);
  if (result.ok && result.command) {
    botForm.config.codebuddy.cliPath = result.command;
    envCheck.state = 'found';
    envCheck.candidates = [{ command: result.command, source: 'path' }];
  } else {
    envCheck.state = 'missing';
    envCheck.installFailed = true;
    envCheck.installOutput = result.output;
  }
}

// 选中后端类型变化时自动检测
watch(() => botForm.backendType, () => {
  if (botForm.visible) {
    void checkBackendEnv();
    // codebuddy 后端选中时自动拉一次模型列表（datalist 候选，用户免手动点 ↻）
    if (botForm.backendType === 'codebuddy') void fetchCodebuddyModels();
  }
});

// 打开表单（新建/编辑）时检测一次
watch(() => botForm.visible, (visible) => {
  if (visible) {
    void checkBackendEnv();
    // 打开时若已是 codebuddy 后端也拉一次（编辑既有 bot 的场景）
    if (botForm.backendType === 'codebuddy') void fetchCodebuddyModels();
  } else {
    envCheck.state = 'idle';
  }
});

// ------------------------------------------------------------------
// 初始化
// ------------------------------------------------------------------

onMounted(async () => {
  // 监听握手失败事件（index.ts 在 connectPluginBridge 失败时派发）
  const onSdkFailed = (e: Event): void => {
    initState.value = 'failed';
    initError.value = (e as CustomEvent<string>).detail || '桥握手失败';
  };
  window.addEventListener('ai-chat:sdk-failed', onSdkFailed);

  try {
    // SDK 就绪是渲染的前提（后续 loadBots 依赖 sdk.docs），必须等待。
    // UI 已挂载（先于握手），用户看到的是加载态而非白屏。
    sdk = await ensurePluginSDK();
  } catch (err) {
    initState.value = 'failed';
    initError.value = err instanceof Error ? err.message : String(err);
    return;
  } finally {
    window.removeEventListener('ai-chat:sdk-failed', onSdkFailed);
  }

  // 数据加载阶段提示（连接已就绪，正在拉 bot 列表）
  initHint.value = '正在加载数据…';

  // 先拉 bot 列表让界面尽快渲染（侧边栏/聊天窗口有了骨架就不算"白屏"）
  await loadBots();

  // 数据就绪，切正常态
  initState.value = 'ready';

  // 默认选中第一个 Bot（用户立即可见聊天界面）
  if (bots.value.length > 0 && !activeBotId.value) {
    selectBot(bots.value[0].id);
  }

  // 注册联系人 + 启动主聊天消息监听是后台维护任务，与界面渲染无关。
  // 且 registerBotContact 是逐个 Tauri invoke 落库，串行 await 会拖慢首屏——
  // 改为 fire-and-forget，内部出错已在 syncBotContactsAndListeners 里逐个捕获打印
  void syncBotContactsAndListeners();
});

// 注：主视图不启动 bot 消息监听（监听统一由后台视图承载），
// 因此此处无需 onUnmounted 清理监听——主视图挂载/卸载不影响 bot 在线状态。

// ------------------------------------------------------------------
// Bot 列表管理
// ------------------------------------------------------------------

async function loadBots(): Promise<void> {
  if (!sdk) return;
  bots.value = await listBots(sdk.docs);
  for (const b of bots.value) {
    console.log(`[ai-chat][loadBots] bot=${b.id} backendConfig=${JSON.stringify(b.backendConfig)}`);
  }
}

function selectBot(botId: string): void {
  activeBotId.value = botId;
  activeBot.value = bots.value.find((b: BotInstance) => b.id === botId) ?? null;
  // 窄窗：选中 Bot 后收起抽屉，聊天区全宽展示
  closeSidebar();
  loadChatHistory();
}

async function loadChatHistory(): Promise<void> {
  if (!sdk || !activeBotId.value) return;
  loadingMessages.value = true;
  try {
    messages.value = await listChatMessages(sdk.docs, activeBotId.value);
  } finally {
    loadingMessages.value = false;
    nextTick(scrollToBottom);
  }
}

// ------------------------------------------------------------------
// Bot 表单
// ------------------------------------------------------------------

function openBotForm(existing?: BotInstance): void {
  if (existing) {
    botForm.editing = true;
    botForm.editingId = existing.id;
    botForm.name = existing.name;
    botForm.backendType = existing.backendType;
    botForm.systemPrompt = existing.systemPrompt ?? '';

    // 从后端配置中恢复对应字段
    const cfg = existing.backendConfig as Record<string, unknown>;
    botForm.config.codebuddy.cliPath = (cfg.cliPath as string) ?? '';
    botForm.config.codebuddy.workdir = (cfg.workdir as string) ?? '';
    botForm.config.codebuddy.model = (cfg.model as string) ?? '';
    botForm.config.openai.baseUrl = (cfg.baseUrl as string) ?? '';
    botForm.config.openai.apiKey = (cfg.apiKey as string) ?? '';
    botForm.config.openai.model = (cfg.model as string) ?? '';
    botForm.config.ollama.endpoint = (cfg.endpoint as string) ?? 'http://localhost:11434';
    botForm.config.ollama.model = (cfg.model as string) ?? '';
  } else {
    Object.assign(botForm, defaultBotForm());
    botForm.visible = true;
    botForm.editing = false;
    botForm.editingId = undefined;
  }
  botForm.visible = true;

  nextTick(() => {
    if (botFormDialogRef.value) {
      botFormDialogRef.value.showModal();
    }
  });
}

function closeBotForm(): void {
  botForm.visible = false;
  if (botFormDialogRef.value) {
    botFormDialogRef.value.close();
  }
}

/** 根据当前表单选择的类型构建 backendConfig */
function buildBackendConfig(): Record<string, unknown> {
  switch (botForm.backendType) {
    case 'codebuddy':
      return {
        type: 'codebuddy',
        cliPath: botForm.config.codebuddy.cliPath || undefined,
        workdir: botForm.config.codebuddy.workdir || undefined,
        model: botForm.config.codebuddy.model || undefined,
      };
    case 'openai':
      return {
        type: 'openai',
        baseUrl: botForm.config.openai.baseUrl,
        apiKey: botForm.config.openai.apiKey,
        model: botForm.config.openai.model,
      };
    case 'ollama':
      return {
        type: 'ollama',
        endpoint: botForm.config.ollama.endpoint,
        model: botForm.config.ollama.model,
      };
    default:
      return {};
  }
}

async function handleSaveBot(): Promise<void> {
  if (!sdk || !botForm.name.trim()) return;

  let botId: string;
  console.log(`[ai-chat][save] 保存配置=${JSON.stringify(buildBackendConfig())}`);

  if (botForm.editing && botForm.editingId) {
    botId = botForm.editingId;
    await updateBot(sdk.docs, botId, {
      name: botForm.name.trim(),
      backendType: botForm.backendType,
      backendConfig: buildBackendConfig(),
      systemPrompt: botForm.systemPrompt.trim() || undefined,
    });
  } else {
    botId = generateId('bot');
    await createBot(sdk.docs, {
      id: botId,
      name: botForm.name.trim(),
      backendType: botForm.backendType,
      backendConfig: buildBackendConfig(),
      systemPrompt: botForm.systemPrompt.trim() || undefined,
    });
  }

  closeBotForm();
  await loadBots();

  // 注册/刷新 bot 联系人名片：新建与编辑都执行（编辑路径刷新通讯录显示名）。
  // 消息监听由后台视图统一承载，主视图不启动监听。
  const savedBot = bots.value.find(b => b.id === botId);
  if (savedBot) {
    try {
      await registerBotContactOnly(sdk, savedBot);
    } catch (err) {
      console.error(`[ai-chat] 注册 bot「${savedBot.name}」联系人失败:`, err);
    }
  }

  // 新创建的 Bot 自动选中
  if (!botForm.editing && activeBotId.value === null) {
    selectBot(bots.value[0]?.id);
  } else if (botForm.editing && activeBotId.value === botForm.editingId) {
    activeBot.value = bots.value.find((b: BotInstance) => b.id === botForm.editingId) ?? null;
  }
}

async function handleDeleteBot(): Promise<void> {
  if (!sdk || !botForm.editingId) return;
  const botId = botForm.editingId;

  // 简单确认（正式版可替换为更友好的确认 UI）
  if (!confirm(`确定删除 Bot "${botForm.name}" 吗？对话历史将被保留在本地，但不会再关联到该 Bot。`)) return;

  // 后台运行时无需显式停用：bot 配置每条消息现读，docs 删除后新消息自然
  // 被忽略（内核路由到插件 → getBot 返回 null → 不回复）

  // 注销联系人：删除通讯录中对应的 bot 好友记录（插件为 bot 生命周期权威源，
  // 删除时同步清理其联系人投影）
  try {
    await sdk.messages?.unregisterAsContact(botRootId(botId));
  } catch (err) {
    // 注销失败不阻塞删除流程（联系人可能本就不存在）
    console.warn('[ai-chat] 注销 bot 联系人失败:', err);
  }

  await deleteBot(sdk.docs, botId);
  closeBotForm();
  await loadBots();

  if (activeBotId.value === botId) {
    activeBotId.value = null;
    activeBot.value = null;
    messages.value = [];
  }
}

// ------------------------------------------------------------------
// 发送消息
// ------------------------------------------------------------------

async function handleSend(event?: KeyboardEvent): Promise<void> {
  // Shift+Enter 换行，仅 Enter 发送
  if (event && event.shiftKey) return;
  if (event) event.preventDefault();

  if (!sdk || !activeBot.value || !inputText.value.trim() || thinking.value) return;

  const text = inputText.value.trim();
  const validation = validateMessage(text);
  if (!validation.ok) {
    alert(validation.reason);
    return;
  }

  thinking.value = true;
  inputText.value = '';

  // 乐观上屏：用户消息
  messages.value.push({
    id: `optimistic-${Date.now()}`,
    botInstanceId: activeBot.value.id,
    role: 'user',
    content: text,
    createdAt: Date.now(),
  });

  // 创建 AI 回复占位符（streaming=true，content 由 processChat 逐字更新）。
  // 用 reactive() 包裹占位对象再入 messages：onToken 逐字改 content 时能触发
  // Vue 响应式渲染；否则直接改原始对象绕过 reactive 代理，流式期间 UI 不更新
  // （评审 Z3）。
  const assistantMsg: ChatMessageRecord = reactive({
    id: generateId('msg'),
    botInstanceId: activeBot.value.id,
    role: 'assistant',
    content: '',
    createdAt: Date.now(),
    streaming: true,
  });
  messages.value.push(assistantMsg);
  nextTick(scrollToBottom);

  try {
    const result = await processChat(sdk.docs, activeBot.value, text, assistantMsg);
    // 替换乐观用户消息为已持久化的真实消息
    const userIdx = messages.value.findIndex(m => m.id.startsWith('optimistic-'));
    if (userIdx >= 0) {
      messages.value[userIdx] = result.userMessage;
    }
    // assistantMsg 已在 messages 中（同一引用），streaming 已由 processChat 关闭
  } catch (error) {
    const errMsg = error instanceof Error ? error.message : String(error);
    assistantMsg.content = errMsg;
    assistantMsg.streaming = false;
    assistantMsg.error = errMsg;
  } finally {
    thinking.value = false;
    nextTick(() => {
      scrollToBottom();
      inputRef.value?.focus();
    });
  }
}

// ------------------------------------------------------------------
// 工具函数
// ------------------------------------------------------------------

function backendLabel(type?: BackendType): string {
  switch (type) {
    case 'codebuddy':
      return 'CodeBuddy CLI';
    case 'openai':
      return 'OpenAI';
    case 'ollama':
      return 'Ollama';
    default:
      return type ?? '未知';
  }
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function scrollToBottom(): void {
  nextTick(() => {
    if (messageListRef.value) {
      messageListRef.value.scrollTop = messageListRef.value.scrollHeight;
    }
  });
}

watch(activeBotId, () => {
  nextTick(scrollToBottom);
});
</script>

<!-- 全局（非 scoped）：锁定文档根容器高度，确保 flex 百分比级联生效 -->
<style>
html, body {
  height: 100%;
  margin: 0;
  overflow: hidden;
}

#app {
  height: 100%;
}
</style>

<style scoped>
/* ================================================================
   ai-chat 插件 · 聊天 UI 样式
   目标：简洁、现代、与 Element Plus 宿主主题兼容
   ================================================================ */

/* 根容器（position:relative：窄窗抽屉/遮罩绝对定位的锚点） */
.ai-chat-root {
  display: flex;
  position: relative;
  height: 100%;
  width: 100%;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
  font-size: 14px;
  color: #1e293b;
  background: #f8fafc;
  overflow: hidden;
}

/* 初始化加载/失败态 */
.ai-chat-root.init-state {
  align-items: center;
  justify-content: center;
}

.init-placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
}

.init-spinner {
  width: 28px;
  height: 28px;
  border: 3px solid #e2e8f0;
  border-top-color: #3b82f6;
  border-radius: 50%;
  animation: init-spin 0.8s linear infinite;
}

@keyframes init-spin {
  to { transform: rotate(360deg); }
}

.init-text {
  margin: 0;
  font-size: 13px;
  color: #64748b;
}

.init-text--error {
  color: #dc2626;
  font-weight: 500;
}

.init-detail {
  margin: 0;
  font-size: 12px;
  color: #94a3b8;
  max-width: 400px;
  text-align: center;
  word-break: break-all;
}

/* ----------------------------------------------------------------
   侧边栏
   ---------------------------------------------------------------- */
.sidebar {
  width: 240px;
  min-width: 240px;
  min-height: 0;
  background: #ffffff;
  border-right: 1px solid #e2e8f0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.sidebar-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px;
  border-bottom: 1px solid #e2e8f0;
  flex-shrink: 0;
}

.sidebar-title {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: #0f172a;
}

.btn-add-bot {
  width: 28px;
  height: 28px;
  border: none;
  border-radius: 6px;
  background: #3b82f6;
  color: #fff;
  font-size: 18px;
  line-height: 28px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s;
}

.btn-add-bot:hover {
  background: #2563eb;
}

/* Bot 列表 */
.bot-list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 4px 0;
}

.bot-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 16px;
  cursor: pointer;
  transition: background 0.1s;
  position: relative;
}

.bot-item:hover {
  background: #f1f5f9;
}

.bot-item.active {
  background: #eff6ff;
  border-right: 3px solid #3b82f6;
}

.bot-avatar {
  width: 36px;
  height: 36px;
  border-radius: 10px;
  overflow: hidden;
  flex-shrink: 0;
}

.bot-avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.bot-avatar-default {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, #3b82f6, #8b5cf6);
  color: #fff;
  font-weight: 600;
  font-size: 16px;
}

.bot-info {
  flex: 1;
  min-width: 0;
}

.bot-name {
  font-weight: 500;
  font-size: 13px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.bot-type {
  font-size: 11px;
  color: #94a3b8;
  margin-top: 1px;
}

.bot-menu-btn {
  width: 24px;
  height: 24px;
  border: none;
  background: transparent;
  color: #94a3b8;
  font-size: 16px;
  cursor: pointer;
  border-radius: 4px;
  flex-shrink: 0;
  line-height: 24px;
  text-align: center;
}

.bot-menu-btn:hover {
  background: #e2e8f0;
  color: #475569;
}

.sidebar-empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: #94a3b8;
  font-size: 13px;
  gap: 4px;
}

.sidebar-empty p {
  margin: 0;
}

.sidebar-empty-hint {
  font-size: 11px;
}

/* ----------------------------------------------------------------
   窄窗布局（≤600px，ui-layout.isNarrowLayout）：
   侧栏折叠为覆盖抽屉，聊天区全宽；顶栏 ☰ 打开，
   遮罩点击 / 头部 × / 选中 Bot 均可收起
   ---------------------------------------------------------------- */

/* 顶栏 ☰ 打开侧栏 */
.btn-sidebar-toggle {
  flex-shrink: 0;
  width: 32px;
  height: 32px;
  margin-right: 8px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: #475569;
  font-size: 16px;
  line-height: 32px;
  cursor: pointer;
}

.btn-sidebar-toggle:hover {
  background: #f1f5f9;
}

/* 未选 Bot 空态的 ☰：钉在窗口左上角 */
.btn-sidebar-toggle--empty {
  position: absolute;
  top: 8px;
  left: 8px;
  z-index: 5;
  margin-right: 0;
}

.sidebar-header-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

/* 抽屉头部 × 收起按钮 */
.btn-sidebar-close {
  width: 28px;
  height: 28px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: #94a3b8;
  font-size: 16px;
  line-height: 28px;
  cursor: pointer;
}

.btn-sidebar-close:hover {
  background: #f1f5f9;
  color: #475569;
}

/* 覆盖抽屉形态：绝对定位滑入滑出；宽窗无 --overlay 类，样式不生效 */
.sidebar--overlay {
  position: absolute;
  top: 0;
  left: 0;
  bottom: 0;
  z-index: 30;
  max-width: 85vw;
  box-shadow: 8px 0 24px rgba(15, 23, 42, 0.12);
  transform: translateX(-105%);
  transition: transform 0.2s ease;
}

.sidebar--overlay.sidebar--open {
  transform: translateX(0);
}

.sidebar-backdrop {
  position: absolute;
  inset: 0;
  z-index: 20;
  background: rgba(15, 23, 42, 0.25);
}

/* ----------------------------------------------------------------
   聊天区
   ---------------------------------------------------------------- */
.chat-area {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

.chat-area--empty {
  align-items: center;
  justify-content: center;
}

.no-bot-selected {
  text-align: center;
  color: #94a3b8;
}

.no-bot-icon {
  font-size: 48px;
  margin: 0 0 12px 0;
}

.no-bot-selected p {
  margin: 0;
  font-size: 15px;
}

/* 聊天头部 */
.chat-header {
  padding: 12px 20px;
  background: #fff;
  border-bottom: 1px solid #e2e8f0;
  min-height: 48px;
  display: flex;
  align-items: center;
  flex-shrink: 0;
}

.chat-header-info strong {
  font-size: 15px;
  font-weight: 600;
}

.chat-header-type {
  font-size: 11px;
  color: #94a3b8;
  margin-left: 8px;
}

/* 消息列表 */
.message-list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 16px 20px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.messages-loading,
.messages-empty {
  text-align: center;
  color: #94a3b8;
  font-size: 13px;
  padding: 40px 0;
}

/* 消息行 */
.message-row {
  display: flex;
  max-width: 85%;
}

.message-row--user {
  align-self: flex-end;
}

.message-row--bot {
  align-self: flex-start;
}

/* 消息气泡 */
.message-bubble {
  padding: 10px 14px;
  border-radius: 12px;
  font-size: 13.5px;
  line-height: 1.6;
  word-break: break-word;
}

.message-bubble--user {
  background: #3b82f6;
  color: #fff;
  border-bottom-right-radius: 4px;
}

.message-bubble--assistant {
  /* 不再使用：AI 消息已改为无气泡布局（.bot-message-block），保留选择器避免旧样式残留 */
  display: none;
}

.message-text {
  white-space: pre-wrap;
}

.message-error {
  margin-top: 6px;
  padding: 6px 8px;
  background: #fef2f2;
  color: #dc2626;
  border-radius: 6px;
  font-size: 12px;
}

.message-meta {
  margin-top: 4px;
  font-size: 10px;
  color: #94a3b8;
}

.message-bubble--user .message-meta {
  color: rgba(255, 255, 255, 0.7);
}

/* AI 消息无气泡布局（ChatGPT/Claude 风格：头像 + 名称 + 文本块） */
.bot-message-block {
  display: flex;
  gap: 10px;
  align-items: flex-start;
}

.bot-msg-avatar {
  flex-shrink: 0;
  width: 30px;
  height: 30px;
  border-radius: 6px;
  overflow: hidden;
  background: #dbeafe;
  display: flex;
  align-items: center;
  justify-content: center;
}

.bot-msg-avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.bot-msg-avatar-default {
  font-size: 13px;
  font-weight: 600;
  color: #3b82f6;
}

.bot-msg-body {
  flex: 1;
  min-width: 0;
}

.bot-msg-name {
  font-size: 11px;
  font-weight: 600;
  color: #64748b;
  margin-bottom: 2px;
}

.bot-msg-text {
  font-size: 13.5px;
  line-height: 1.6;
  color: #1e293b;
  white-space: pre-wrap;
  word-break: break-word;
}

/* 流式光标闪烁动画 */
.streaming-cursor {
  animation: cursorBlink 0.8s infinite;
  color: #3b82f6;
  font-weight: bold;
}

@keyframes cursorBlink {
  0%, 50% { opacity: 1; }
  51%, 100% { opacity: 0; }
}

.bot-msg-content .message-html {
  font-size: 13.5px;
  line-height: 1.6;
  color: #1e293b;
}

/* Markdown 渲染内容 */
.message-html :deep(p) {
  margin: 0.25em 0;
}

.message-html :deep(p:first-child) {
  margin-top: 0;
}

.message-html :deep(p:last-child) {
  margin-bottom: 0;
}

.message-html :deep(pre) {
  margin: 6px 0;
  padding: 10px 12px;
  background: #f1f5f9;
  border-radius: 6px;
  overflow-x: auto;
  font-size: 12px;
  line-height: 1.5;
}

.message-html :deep(code) {
  font-family: 'Menlo', 'Monaco', 'Courier New', monospace;
  font-size: 12px;
}

.message-html :deep(p code) {
  padding: 1px 4px;
  background: #f1f5f9;
  border-radius: 3px;
}

.message-html :deep(pre code) {
  padding: 0;
  background: transparent;
}

.message-html :deep(ul),
.message-html :deep(ol) {
  margin: 4px 0;
  padding-left: 20px;
}

.message-html :deep(li) {
  margin: 2px 0;
}

.message-html :deep(h1),
.message-html :deep(h2),
.message-html :deep(h3),
.message-html :deep(h4),
.message-html :deep(h5),
.message-html :deep(h6) {
  margin: 8px 0 4px 0;
  line-height: 1.3;
}

.message-html :deep(blockquote) {
  margin: 4px 0;
  padding: 4px 10px;
  border-left: 3px solid #e2e8f0;
  color: #64748b;
}

.message-html :deep(hr) {
  margin: 8px 0;
  border: none;
  border-top: 1px solid #e2e8f0;
}

.message-html :deep(strong) {
  font-weight: 600;
}

/* 思考中状态已由流式占位（.bot-msg-text.streaming）替代 */
/* 保留 dotPulse keyframes（若有）以备他用 */


@keyframes dotPulse {
  0%, 80%, 100% {
    transform: scale(0.6);
    opacity: 0.4;
  }
  40% {
    transform: scale(1);
    opacity: 1;
  }
}

/* 输入区域 */
.chat-input-area {
  display: flex;
  gap: 10px;
  padding: 12px 20px 16px;
  background: #fff;
  border-top: 1px solid #e2e8f0;
  align-items: flex-end;
  flex-shrink: 0;
}

.chat-input {
  flex: 1;
  border: 1px solid #e2e8f0;
  border-radius: 10px;
  padding: 10px 14px;
  font-size: 13px;
  font-family: inherit;
  resize: none;
  outline: none;
  line-height: 1.5;
  max-height: 120px;
  transition: border-color 0.15s;
}

.chat-input:focus {
  border-color: #3b82f6;
}

.chat-input:disabled {
  background: #f8fafc;
}

.btn-send {
  padding: 10px 20px;
  background: #3b82f6;
  color: #fff;
  border: none;
  border-radius: 10px;
  font-size: 13px;
  font-weight: 500;
  cursor: pointer;
  white-space: nowrap;
  transition: background 0.15s;
}

.btn-send:hover:not(:disabled) {
  background: #2563eb;
}

.btn-send:disabled {
  background: #94a3b8;
  cursor: not-allowed;
}

/* ----------------------------------------------------------------
   Bot 表单弹窗
   ---------------------------------------------------------------- */
.bot-form-dialog {
  border: none;
  border-radius: 14px;
  padding: 0;
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.15);
  max-width: 460px;
  width: 90vw;
}

.bot-form-dialog::backdrop {
  background: rgba(0, 0, 0, 0.3);
}

.dialog-content {
  padding: 24px;
}

.dialog-content h3 {
  margin: 0 0 20px 0;
  font-size: 16px;
  font-weight: 600;
}

.form-field {
  display: block;
  margin-bottom: 14px;
}

.form-label {
  display: block;
  font-size: 12px;
  font-weight: 500;
  color: #64748b;
  margin-bottom: 4px;
}

.form-input {
  width: 100%;
  padding: 8px 12px;
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  font-size: 13px;
  font-family: inherit;
  outline: none;
  transition: border-color 0.15s;
  box-sizing: border-box;
}

.form-input:focus {
  border-color: #3b82f6;
}

select.form-input {
  cursor: pointer;
}

.form-textarea {
  resize: vertical;
  min-height: 60px;
}

.form-hint {
  margin: -8px 0 14px 0;
  font-size: 11px;
  color: #94a3b8;
}

/* 模型选择器：输入框 + 刷新按钮 */
.model-select-row {
  display: flex;
  gap: 6px;
  align-items: center;
}

.model-select-row .form-input {
  flex: 1;
  margin-bottom: 0;
}

.btn-model-refresh {
  flex-shrink: 0;
  width: 32px;
  height: 32px;
  padding: 0;
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  background: #fff;
  color: #64748b;
  font-size: 16px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: border-color 0.15s, color 0.15s;
}

.btn-model-refresh:hover:not(:disabled) {
  border-color: #3b82f6;
  color: #3b82f6;
}

.btn-model-refresh:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.form-hint code {
  font-size: 11px;
  padding: 1px 4px;
  background: #f1f5f9;
  border-radius: 3px;
}

/* 后端环境检测块 */
.env-check {
  margin: 0 0 14px 0;
  padding: 10px 12px;
  background: #f8fafc;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
}

.env-check-line {
  margin: 0 0 8px 0;
  font-size: 12px;
}

.env-check-line--info {
  color: #64748b;
}

.env-check-line--ok {
  color: #16a34a;
}

.env-check-line--warn {
  color: #d97706;
}

.env-check-line--error {
  color: #dc2626;
}

/* 多候选选择列表 */
.env-candidates {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 4px 0 8px 0;
}

.env-candidate {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border: 1px solid #e2e8f0;
  border-radius: 5px;
  cursor: pointer;
  background: #fff;
}

.env-candidate:hover {
  border-color: #93c5fd;
}

.env-candidate-name {
  font-family: ui-monospace, monospace;
  font-size: 12px;
}

.env-candidate-src {
  margin-left: auto;
  font-size: 11px;
  color: #94a3b8;
}

/* 手动路径输入行 */
.env-manual {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}

.env-manual .form-input {
  flex: 1;
  margin-bottom: 0;
}

/* 一键安装分隔 */
.env-install {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.env-install-or {
  font-size: 12px;
  color: #94a3b8;
}

.env-check-output {
  margin: 8px 0 0 0;
  padding: 8px;
  font-size: 11px;
  background: #fff;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  max-height: 120px;
  overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-all;
}

.btn--small {
  padding: 4px 12px;
  font-size: 12px;
  margin-bottom: 8px;
}

.dialog-actions {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 8px;
}

.dialog-actions-right {
  display: flex;
  gap: 8px;
  margin-left: auto;
}

.btn-confirm {
  padding: 8px 20px;
  background: #3b82f6;
  color: #fff;
  border: none;
  border-radius: 8px;
  font-size: 13px;
  font-weight: 500;
  cursor: pointer;
  transition: background 0.15s;
}

.btn-confirm:hover:not(:disabled) {
  background: #2563eb;
}

.btn-confirm:disabled {
  background: #94a3b8;
  cursor: not-allowed;
}

.btn-cancel {
  padding: 8px 16px;
  background: transparent;
  color: #64748b;
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  font-size: 13px;
  cursor: pointer;
}

.btn-cancel:hover {
  background: #f1f5f9;
}

.btn-danger {
  padding: 8px 14px;
  background: transparent;
  color: #dc2626;
  border: 1px solid #fecaca;
  border-radius: 8px;
  font-size: 12px;
  cursor: pointer;
}

.btn-danger:hover {
  background: #fef2f2;
}
</style>
