/**
 * 应用启用状态：per-space 单一事实源（install-and-enable.md §一/§二/§五；
 * problem.md L4/M4/D4 口径——安装是系统层本机动作，「在哪个空间启用」是空间层实例）。
 *
 * 口径：
 * - 键＝空间 × 插件。空间键沿用 spaceKeyOf 唯一约定：个人空间 'personal'，
 *   组织空间 'org:<orgId>'；localStorage 键 `spark:apps-enabled:<spaceKey>`，
 *   值为 Record<pluginId, boolean>（仅记录显式启停，缺席走默认语义）。
 * - 默认语义（install-and-enable §二「组织成立即具备」）：内置件默认在其
 *   supportedSpaces 声明的所有空间启用；非内置默认未启用。
 * - 个人空间过渡种子：per-space 记录缺席时回退内核全局开关（市场条目 enabled，
 *   即历史启用事实）；启用/停用写本 store 并回写内核 pluginMarket.setEnabled
 *   （运行时要），UI 一律以本 store 为事实源。
 *
 * 过渡期诚实声明：本 store 是**本机**存储。组织空间的启用清单按产品口径应由
 * 内核「组织启用清单」接管——变更走组织事务（决议立法）、随组织加密副本全体
 * 一致同步（install-and-enable §五）；在内核落地前，组织空间的启停仅在
 * 本机生效，成员间不同步，UI 已如实标注，不虚构「组织已同步」。
 *
 * 内置件判定（过渡期）：内核市场 DTO 尚未透出安装 trust 字段（trust="builtin"
 * 经 trustLevel 派生后与侧载同为 L0，不可区分），此处按随发行包预装的插件 id
 * 集合判定（与 code/app/src-tauri/resources/builtin-plugins 对齐）；内核透出
 * trust 后改读 DTO，删除本集合。
 */
import { ref } from 'vue';
import type { PluginMarketItemDto, PluginSpaceType } from '../api/types';
import { DEFAULT_SUPPORTED_SPACES } from '../components/apps/space-visibility';

/** 启用判定所需的最小应用描述（市场条目结构子集，mock/合成条目同形） */
export type EnablementAppRef = Pick<PluginMarketItemDto, 'id' | 'supportedSpaces' | 'enabled'>;

/** 空间引用（与 current-space 的 CurrentSpace 同形；store 不反向依赖 current-space） */
export type EnablementSpace = { type: 'personal' } | { type: 'org'; orgId: string };

const STORAGE_PREFIX = 'spark:apps-enabled:';
/** 旧组织启用 mock（apps-store useOrgEnabled，已退役）：AppsPage 写入侧用裸 orgId，
 *  app-conversations 读取侧用 'org:<orgId>'——两种历史键都做一次性的读取迁移 */
const LEGACY_ORG_MOCK_PREFIX = 'spark:apps-org-enabled:';

/** 随发行包预装的内置件（见模块头注释；trust="builtin" 的 renderer 侧镜像） */
const BUILTIN_PLUGIN_IDS: ReadonlySet<string> = new Set([
  'spark-chat',
  'spark-contacts',
  'spark-market',
  // A42：组织管理（灰度宿主外也作为空间桌面应用出现）与文件管理
  'spark-org-admin',
  'spark-files'
]);

export function isBuiltinApp(pluginId: string): boolean {
  return BUILTIN_PLUGIN_IDS.has(pluginId);
}

/** 空间键（spaceKeyOf 唯一约定的本地副本：store 不依赖 mock 层，避免分层倒置） */
export function enablementSpaceKey(space: EnablementSpace): string {
  return space.type === 'org' ? `org:${space.orgId}` : 'personal';
}

function storageKeyOf(spaceKey: string): string {
  return `${STORAGE_PREFIX}${spaceKey}`;
}

function loadJson(key: string): Record<string, boolean> | null {
  try {
    const raw = localStorage.getItem(key);
    if (raw) {
      return JSON.parse(raw) as Record<string, boolean>;
    }
  } catch {
    // 本地存储不可读/数据损坏时按无记录处理
  }
  return null;
}

/** 加载某空间的启用记录：新键优先；缺席时一次性迁移旧组织 mock（两种历史键形，
 *  后者覆盖前者）。迁移只在内存生效，首次显式启停时才落新键（不改写旧数据）。 */
function loadSpaceRecord(spaceKey: string): Record<string, boolean> {
  const current = loadJson(storageKeyOf(spaceKey));
  if (current) {
    return current;
  }
  if (spaceKey.startsWith('org:')) {
    const orgId = spaceKey.slice('org:'.length);
    return {
      ...(loadJson(`${LEGACY_ORG_MOCK_PREFIX}${orgId}`) ?? {}),
      ...(loadJson(`${LEGACY_ORG_MOCK_PREFIX}${spaceKey}`) ?? {})
    };
  }
  return {};
}

/** 全量记录（spaceKey → pluginId → enabled）；ref 替换式更新保证 computed 读取方响应式 */
const recordsBySpace = ref<Record<string, Record<string, boolean>>>({});
const loadedSpaceKeys = new Set<string>();

/** 懒加载某空间记录进响应式缓存（幂等；读取入口内部都会先走这里） */
export function ensureEnablementLoaded(space: EnablementSpace): void {
  const spaceKey = enablementSpaceKey(space);
  if (loadedSpaceKeys.has(spaceKey)) {
    return;
  }
  loadedSpaceKeys.add(spaceKey);
  recordsBySpace.value = { ...recordsBySpace.value, [spaceKey]: loadSpaceRecord(spaceKey) };
}

/** 默认语义：内置件默认在 supportedSpaces 声明的所有空间启用（组织成立即具备）；
 *  非内置默认未启用——个人空间过渡期内回退内核全局开关作为种子（见模块头注释） */
export function defaultEnabledInSpace(space: EnablementSpace, app: EnablementAppRef): boolean {
  const spaces =
    app.supportedSpaces && app.supportedSpaces.length > 0 ? app.supportedSpaces : DEFAULT_SUPPORTED_SPACES;
  if (isBuiltinApp(app.id)) {
    return spaces.includes(space.type);
  }
  return space.type === 'personal' ? app.enabled : false;
}

/** 应用在某空间是否已启用（UI 唯一事实源；桌面注册表/启动器/消息卡片同读此判定） */
export function isAppEnabledInSpace(space: EnablementSpace, app: EnablementAppRef): boolean {
  ensureEnablementLoaded(space);
  const record = recordsBySpace.value[enablementSpaceKey(space)];
  if (record && Object.prototype.hasOwnProperty.call(record, app.id)) {
    return record[app.id];
  }
  return defaultEnabledInSpace(space, app);
}

/** 显式启停某空间内的应用（写本机 per-space 记录；组织空间的内核接管见模块头注释）。
 *  个人空间的内核全局开关回写（pluginMarket.setEnabled）由调用方完成——本 store 不
 *  依赖 electronAPI，保持纯本地可测。 */
export function setAppEnabledInSpace(space: EnablementSpace, pluginId: string, enabled: boolean): void {
  ensureEnablementLoaded(space);
  const spaceKey = enablementSpaceKey(space);
  const next = { ...(recordsBySpace.value[spaceKey] ?? {}), [pluginId]: enabled };
  recordsBySpace.value = { ...recordsBySpace.value, [spaceKey]: next };
  try {
    localStorage.setItem(storageKeyOf(spaceKey), JSON.stringify(next));
  } catch {
    // 持久化失败不阻断交互（重启后回退默认语义）
  }
}

/** 应用在某空间是否可启用（supportedSpaces 声明含该空间类型；缺省按 ['org']） */
export function isAppEnableableInSpace(space: EnablementSpace, app: EnablementAppRef): boolean {
  const spaces: PluginSpaceType[] =
    app.supportedSpaces && app.supportedSpaces.length > 0 ? app.supportedSpaces : DEFAULT_SUPPORTED_SPACES;
  return spaces.includes(space.type);
}
