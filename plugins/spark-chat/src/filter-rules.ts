/**
 * 个人过滤规则（communication §4.3，product/todo #22①）：关键词 / 来源屏蔽。
 *
 * 自我审查定位（如实口径，UI 文案同此）：**纯本地过滤**——规则只影响本机
 * 当前账号自己的聊天视图（渲染前过滤），不拦截投递、不回写内核、不影响
 * 对方与网络中的任何人。规则存插件自身数据域（sdk.data declareCollection，
 * 集合 scope:"local" = 数据不离开本机，wiki plugin-data-api §2）；换插件 /
 * 换设备经 JSON 导入导出携带（导入校验结构，畸形拒绝不崩）。
 *
 * 状态模式与 store.ts 同口径：模块级响应式缓存 + 首次访问异步水合（同步
 * 返回缓存，水合完成后响应式自动刷新）；写操作本地同步生效后
 * fire-and-forget 持久化（静默 catch，失败不回滚本地态）。未绑定 SDK
 * （vitest / 纯前端预览）不发任何调用，退化为纯内存。
 */
import { reactive } from 'vue';
import { dataApi } from './sdk-host';
import type { ChatMessage } from './store';

export type FilterRuleKind = 'keyword' | 'source';

export interface FilterRule {
  id: string;
  kind: FilterRuleKind;
  /** keyword：匹配文本（大小写不敏感子串）；source：发送方 senderId 精确匹配 */
  target: string;
  enabled: boolean;
  createdAt: number;
}

/** 插件数据域集合（plugin-data-api §2：scope local = 数据不离开本机） */
export const FILTER_RULES_COLLECTION = 'spark-chat:filter-rules';

/** 导出文件结构标记与版本（导入校验的前两道闸） */
export const FILTER_RULES_EXPORT_FORMAT = 'spark-chat-filter-rules';
export const FILTER_RULES_EXPORT_VERSION = 1;

// ---------- 响应式缓存 ----------

const state = reactive<{ rules: FilterRule[] }>({ rules: [] });
/** 水合只触发一次（declareCollection → query 全量读回） */
let hydrateStarted = false;
/** 本地生成规则 id 的自增序号（id 形如 `fr${Date.now()}-${seq}`） */
let seq = 0;

/**
 * 清空规则缓存（登录态切换 / 测试用）：模块级单例跨窗口会话存活，
 * 换账号后旧账号规则不能带进新会话——清空后重新水合。
 */
export function resetFilterRulesCache(): void {
  state.rules = [];
  hydrateStarted = false;
}

// ---------- 持久化（fire-and-forget，与 store.ts 同口径） ----------

function persistSave(rule: FilterRule): void {
  void dataApi()
    ?.save(FILTER_RULES_COLLECTION, rule.id, { ...rule })
    .catch(() => {});
}

function persistDelete(id: string): void {
  void dataApi()
    ?.delete(FILTER_RULES_COLLECTION, id)
    .catch(() => {});
}

/** 首次访问时声明集合并读回全量规则；按 id merge，保留水合期间本地新增的规则 */
function ensureHydrated(): void {
  if (hydrateStarted) return;
  hydrateStarted = true;
  const api = dataApi();
  if (!api) return;
  void (async () => {
    try {
      await api.declareCollection({ name: FILTER_RULES_COLLECTION, scope: 'local' });
      // 规则量级为几十条，单页上限 2000 足够；超限截断属保守降级（不丢本地态）
      const { items } = await api.query<FilterRule>(FILTER_RULES_COLLECTION, { limit: 2000 });
      const persisted: FilterRule[] = [];
      for (const item of items) {
        // 持久化数据同样按不信任输入逐字段校验（与导入同函数），坏项跳过不崩
        const rule = normalizeRule(item.value);
        if (rule) persisted.push(rule);
      }
      const localOnly = state.rules.filter((r) => !persisted.some((p) => p.id === r.id));
      state.rules = [...persisted, ...localOnly].sort((a, b) => a.createdAt - b.createdAt);
    } catch {
      // 持久化通路失败不退化本地行为：内存规则继续生效（下次启动重试水合）
      hydrateStarted = false;
    }
  })();
}

// ---------- CRUD ----------

/** 规则列表（首次访问触发水合，与 store 的 ensureSpace 同模式） */
export function listFilterRules(): FilterRule[] {
  ensureHydrated();
  return state.rules;
}

function addRule(kind: FilterRuleKind, target: string): FilterRule | undefined {
  const trimmed = target.trim();
  if (!trimmed) return undefined;
  ensureHydrated();
  // 幂等：同 kind+target 不重复建（已禁用也不复活，状态变更走 setRuleEnabled）
  const existing = state.rules.find((r) => r.kind === kind && r.target === trimmed);
  if (existing) return existing;
  const rule: FilterRule = {
    id: `fr${Date.now()}-${++seq}`,
    kind,
    target: trimmed,
    enabled: true,
    createdAt: Date.now()
  };
  state.rules = [...state.rules, rule];
  persistSave(rule);
  return rule;
}

/** 新增关键词规则：消息内容含该词（大小写不敏感）即过滤 */
export function addKeywordRule(keyword: string): FilterRule | undefined {
  return addRule('keyword', keyword);
}

/** 新增来源屏蔽规则：senderId 精确匹配的消息一律过滤 */
export function addSourceRule(senderId: string): FilterRule | undefined {
  return addRule('source', senderId);
}

export function setRuleEnabled(id: string, enabled: boolean): void {
  const rule = state.rules.find((r) => r.id === id);
  if (!rule || rule.enabled === enabled) return;
  const next = { ...rule, enabled };
  state.rules = state.rules.map((r) => (r.id === id ? next : r));
  persistSave(next);
}

export function removeFilterRule(id: string): void {
  if (!state.rules.some((r) => r.id === id)) return;
  state.rules = state.rules.filter((r) => r.id !== id);
  persistDelete(id);
}

/** 清空全部规则（导出→清除→导入还原链路的「清除」段） */
export function clearFilterRules(): void {
  const ids = state.rules.map((r) => r.id);
  state.rules = [];
  for (const id of ids) persistDelete(id);
}

// ---------- 渲染前过滤 ----------

/**
 * 消息是否被过滤（渲染前调用）：任一启用规则命中即过滤（关键词与来源
 * 规则为或语义，叠加越多过滤越宽）。自己发出的消息不过滤——否则用户
 * 会看不见自己刚发出的内容（关键词命中自己的消息属预期内正常行为）。
 */
export function isMessageFiltered(msg: Pick<ChatMessage, 'senderId' | 'content'>): boolean {
  if (msg.senderId === 'me') return false;
  const rules = listFilterRules();
  const content = msg.content.toLowerCase();
  for (const rule of rules) {
    if (!rule.enabled) continue;
    if (rule.kind === 'source' && msg.senderId === rule.target) return true;
    if (rule.kind === 'keyword' && content.includes(rule.target.toLowerCase())) return true;
  }
  return false;
}

// ---------- 导入导出（换插件可携带，communication §4.3） ----------

interface FilterRulesExportFile {
  format: string;
  version: number;
  rules: FilterRule[];
}

/** 导出为 JSON 文本（UI 落文件 / 剪贴板；结构化两格缩进便于人工检视） */
export function exportFilterRules(): string {
  const file: FilterRulesExportFile = {
    format: FILTER_RULES_EXPORT_FORMAT,
    version: FILTER_RULES_EXPORT_VERSION,
    rules: listFilterRules()
  };
  return JSON.stringify(file, null, 2);
}

/** 逐字段校验单条规则（导入与水合共用：外部输入一律不信任） */
function normalizeRule(raw: unknown): FilterRule | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.id !== 'string' || !r.id) return null;
  if (r.kind !== 'keyword' && r.kind !== 'source') return null;
  if (typeof r.target !== 'string' || !r.target.trim()) return null;
  if (typeof r.enabled !== 'boolean') return null;
  if (typeof r.createdAt !== 'number' || !Number.isFinite(r.createdAt)) return null;
  return { id: r.id, kind: r.kind, target: r.target, enabled: r.enabled, createdAt: r.createdAt };
}

export type ParseImportResult = { ok: true; rules: FilterRule[] } | { ok: false; reason: string };

/** 解析并校验导入文本（只校验不落库）：结构非法返回 ok:false + 原因，不抛异常 */
export function parseFilterRulesImport(text: string): ParseImportResult {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { ok: false, reason: '不是合法的 JSON 文本' };
  }
  if (typeof raw !== 'object' || raw === null) {
    return { ok: false, reason: '文件结构不是 JSON 对象' };
  }
  const file = raw as Partial<FilterRulesExportFile>;
  if (file.format !== FILTER_RULES_EXPORT_FORMAT) {
    return { ok: false, reason: '不是聊天过滤规则导出文件（format 不匹配）' };
  }
  if (file.version !== FILTER_RULES_EXPORT_VERSION) {
    return { ok: false, reason: `不支持的导出版本（${String(file.version)}）` };
  }
  if (!Array.isArray(file.rules)) {
    return { ok: false, reason: '文件缺少 rules 数组' };
  }
  const rules: FilterRule[] = [];
  const seen = new Set<string>();
  for (const item of file.rules) {
    const rule = normalizeRule(item);
    if (!rule) return { ok: false, reason: '存在结构非法的规则项' };
    if (seen.has(rule.id)) return { ok: false, reason: `规则 id 重复（${rule.id}）` };
    seen.add(rule.id);
    rules.push(rule);
  }
  return { ok: true, rules };
}

export type ImportResult = { ok: true; imported: number; skipped: number } | { ok: false; reason: string };

/**
 * 导入合并：按 id 去重——本地已存在同 id 规则时跳过不覆盖（本地状态优先，
 * 避免旧导出文件抹掉本地新改动）；新规则追加并逐条持久化。
 */
export function importFilterRules(text: string): ImportResult {
  const parsed = parseFilterRulesImport(text);
  if (!parsed.ok) return parsed;
  ensureHydrated();
  let imported = 0;
  let skipped = 0;
  for (const rule of parsed.rules) {
    if (state.rules.some((r) => r.id === rule.id)) {
      skipped += 1;
      continue;
    }
    state.rules = [...state.rules, rule];
    persistSave(rule);
    imported += 1;
  }
  return { ok: true, imported, skipped };
}
