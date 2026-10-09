/**
 * 代码仓库应用（spark-git-repo）· 业务服务层。
 *
 * 职责边界（git-repo.md v0.2 + 拍板口径）：
 * - 镜像清单 = 项目议题内的签名事务操作（档二-5，git.mirror.manifest）；
 *   逐 Git 对象散 blob（档三-6，内容为 git 对象原始字节，cid 入清单）；
 * - 浏览纯 JS 解析镜像对象（不经 git CLI）——桌面/移动同码，移动端天然只读可用；
 * - PR = 子事务（refs parent → 项目议题）+ bundle 附件（sdk.content，cid 入操作）；
 *   操作集 pr.open/update/comment/review/merged/closed 全部签名入日志；
 * - 合并 = 单维护者合并回执即生效（档一-3），禁止非快进写回（档一-3/O7，
 *   checkFastForward 纯函数判定 + CLI merge-base 复核）；「已合并」可验证判据
 *   = 回执 commit ∈ 新镜像历史且包含 PR head（verifyMergeReceipt，任何节点可重算）；
 * - 终态写权校验（档一-3/§3.2）：mergePr/closePr 提交前 readRules 项目议题取
 *   写权集合（rules.maintainers），当前 actor ∉ 集合即拒（fail-closed）；
 *   derivePrState 提供 writeSet 时只采纳集合内 actor 的 merged/closed，集合外
 *   终态操作留 timeline 并如实标注「未采纳」；
 * - 写路径（发布镜像/发起 PR/合并/物化）仅桌面：sdk.sys 缺席或 UA 移动端 →
 *   视图层隐藏入口（mobileReadonly 口径；壳层 enforcement 线形未落地，插件自查）；
 * - 签名主体诚实口径（同 spark-affairs）：actor 为本插件域身份，不代表个人身份。
 *
 * 库包形态（档二-3 组合纪律）：本层不假设自身插件域身份——本机集合名经
 * gitRepoCollections(namespace) 构造；被「项目」等组合者构建期复用时，组合者
 * 以自身插件 id 作 namespace 构造 GitRepoService，数据写组合者命名空间
 * （内核 plugindata 强制集合名前缀 == 调用方插件 id）。
 */

import type {
  AffairGenesisInput,
  AffairLogEntry,
  AffairOpStatus,
  PluginAffairsAPI,
  PluginContentAPI,
  PluginSDK
} from '../../packages/plugin-sdk/src';
import {
  checkoutBranch,
  createBundleBase64,
  deleteRef,
  detectGitCli,
  fetchBundle,
  isAncestorCli,
  listLocalBranches,
  listReachableObjects,
  materializeRepo,
  mergeCommit,
  mergeFfOnly,
  pickDefaultBranch,
  readObjectBase64,
  removeFile,
  resolveRef,
  writeFileBase64,
  type ExecFn
} from './git';
import {
  PR_AFFAIR_TYPE,
  base64ToBytes,
  bytesToBase64,
  bytesToUtf8,
  derivePrState,
  diffTrees,
  extractWriteSet,
  latestMirrorManifest,
  listCommits,
  lookupPath,
  looksBinary,
  mirrorSyncStatus,
  parseMirrorManifest,
  parseTree,
  readCommit,
  validateManifestInput,
  validatePrOpenInput,
  verifyMergeReceipt,
  type FileChange,
  type GitObject,
  type GitObjectStore,
  type MirrorManifest,
  type MirrorSyncStatus,
  type ParsedCommit,
  type PrAttachment,
  type PrReviewVerdict,
  type PrState,
  type TreeEntry
} from './model';
import {
  buildMirrorManifestPayload,
  buildOpDraft,
  buildParentRef,
  buildPrClosedPayload,
  buildPrCommentPayload,
  buildPrMergedPayload,
  buildPrOpenPayload,
  buildPrReviewPayload,
  buildPrUpdatePayload,
  deriveIdentity,
  signPayload,
  type AffairActor
} from './wire';

/** 本插件用到的 SDK 模块方法核对清单（fail-fast 点名缺失，同 spark-affairs 纪律） */
export const REQUIRED_AFFAIRS_METHODS = [
  'create', 'follow', 'unfollow', 'listFollowed', 'submitOp', 'readLog', 'readRules', 'onChange'
] as const;
export const REQUIRED_CONTENT_METHODS = [
  'saveBlob', 'readBlob', 'fetchBlob', 'listBlobs', 'pinRoot', 'unpinRoot'
] as const;

export const AFFAIRS_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.affairs（事务模块）——本插件的镜像清单与 PR 子事务均依赖事务容器，缺失时整体不可用。';
export const CONTENT_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.content（内容面 blob 模块）——镜像对象与 PR 附件无法存取，本插件不可用。';

export type PluginCapabilities = {
  affairs: boolean;
  content: boolean;
  /** 桌面写路径（git CLI）：sdk.sys 存在才可能；UA 移动端恒 false */
  desktopWrite: boolean;
  /** 移动端只读（mobileReadonly 口径：写入口一律隐藏而非禁用报错） */
  mobileReadonly: boolean;
  messages: boolean;
};

export function hasModule<T extends object>(mod: Partial<T> | undefined, methods: readonly string[]): mod is T {
  return Boolean(mod && methods.every((method) => typeof (mod as Record<string, unknown>)[method] === 'function'));
}

/** UA 粗判移动端（壳层 platform 注入线形未落地前的自查口径，档二-9） */
export function isMobileUa(): boolean {
  return typeof navigator !== 'undefined' && /Android|iPhone|iPad|iPod|Mobile/i.test(navigator.userAgent);
}

export function probeCapabilities(sdk: PluginSDK): PluginCapabilities {
  const mobile = isMobileUa();
  return {
    affairs: hasModule<PluginAffairsAPI>(sdk.affairs, REQUIRED_AFFAIRS_METHODS),
    content: hasModule<PluginContentAPI>(sdk.content, REQUIRED_CONTENT_METHODS),
    desktopWrite: !mobile && typeof sdk.sys?.exec === 'function',
    mobileReadonly: mobile,
    messages: typeof sdk.messages?.sendAppMessage === 'function'
  };
}

/**
 * 集合名工厂（库包形态，档二-3 组合纪律）。内核 plugindata 强制集合名前缀 ==
 * 调用方插件 id，故前缀不能硬编码：独立安装形态 = 'spark-git-repo'（缺省）；
 * 被「项目」等插件构建期组合（库包形态）时组合者须以其自身插件 id 构造
 * GitRepoService（如 namespace='spark-project'），数据落到组合者命名空间——
 * 库无自己的数据域（sdk.domain 为组合者域）。
 */
export type GitRepoCollections = {
  /** 项目议题绑定（lww；本机偏好，非共享数据） */
  bindings: string;
  /** 已知 PR 登记（本机再发现兜底；SDK 无子事务发现面，见报告缺口） */
  knownPrs: string;
};

export function gitRepoCollections(namespace: string): GitRepoCollections {
  return {
    bindings: `${namespace}:bindings`,
    knownPrs: `${namespace}:known-prs`
  };
}

/** 独立安装形态的缺省集合名（本插件域上下文使用） */
export const GIT_REPO_COLLECTIONS: GitRepoCollections = gitRepoCollections('spark-git-repo');

const BINDING_DOC_ID = 'current';

export type ProjectBinding = { projectAffairId: string; repoName: string };

export type MirrorView = {
  manifest: MirrorManifest;
  opHash: string;
  /** 同 version 冲突数（>0 = 有维护者违背串行合并约定的客观证据，如实呈现） */
  conflicts: number;
  status: MirrorSyncStatus;
};

export type PrSummaryView = {
  affairId: string;
  state: PrState;
};

function nonNull<T>(value: T | null): value is T {
  return value !== null;
}

export class GitRepoService {
  private readonly affairs: PluginAffairsAPI;
  private readonly content: PluginContentAPI;
  private readonly execFn: ExecFn | null;
  private actor: AffairActor | null = null;
  private collectionsReady = false;
  /** 集合名（按命名空间构造；被组合时 = 组合者插件 id，见 gitRepoCollections） */
  private readonly collections: GitRepoCollections;

  constructor(private readonly sdk: PluginSDK, execOverride?: ExecFn, namespace = 'spark-git-repo') {
    if (!hasModule<PluginAffairsAPI>(sdk.affairs, REQUIRED_AFFAIRS_METHODS)) {
      throw new Error(AFFAIRS_MODULE_MISSING);
    }
    if (!hasModule<PluginContentAPI>(sdk.content, REQUIRED_CONTENT_METHODS)) {
      throw new Error(CONTENT_MODULE_MISSING);
    }
    this.affairs = sdk.affairs;
    this.content = sdk.content;
    this.execFn = execOverride ?? (sdk.sys ? (program, args, workdir) => sdk.sys!.exec(program, args, workdir) : null);
    this.collections = gitRepoCollections(namespace);
  }

  static capabilities(sdk: PluginSDK): PluginCapabilities {
    return probeCapabilities(sdk);
  }

  get viewerIdentity(): string | null {
    return this.actor?.identity ?? null;
  }

  // ------------------------------------------------------------------
  // 基础：actor / 签名 / 操作提交 / 本地集合
  // ------------------------------------------------------------------

  private async ensureActor(): Promise<AffairActor> {
    if (this.actor) {
      return this.actor;
    }
    const probe = await this.sdk.identity.sign('spark-git-repo:actor-probe');
    this.actor = { kind: 'person', identity: deriveIdentity(probe.publicKey), publicKey: probe.publicKey };
    return this.actor;
  }

  /** 操作签名（内核入站硬要求；用户拒绝授权 = 提交失败上抛，不降级） */
  private async signRecord(record: Record<string, unknown>): Promise<Record<string, unknown>> {
    const signed = await this.sdk.identity.sign(signPayload(record));
    return { ...record, sig: signed.signature };
  }

  private async submitPayload(
    affairId: string,
    payload: Record<string, unknown>
  ): Promise<{ opHash: string; status: AffairOpStatus }> {
    const actor = await this.ensureActor();
    const log = await this.affairs.readLog(affairId);
    const draft = buildOpDraft(affairId, actor, payload, log.heads, Date.now());
    const result = await this.affairs.submitOp(await this.signRecord(draft));
    return { opHash: result.opHash, status: result.status };
  }

  // ------------------------------------------------------------------
  // 写权集合（档一-3/§3.2：终态操作 merged/closed 的操作者须 ∈ 议题规则写权集合）
  // ------------------------------------------------------------------

  /** 读项目议题现行规则文档的写权集合；规则不可读 → 上抛（写路径 fail-closed） */
  private async writeSetOf(projectAffairId: string): Promise<ReadonlySet<string>> {
    const view = await this.affairs.readRules(projectAffairId);
    return new Set(extractWriteSet(view.current.rules));
  }

  /** 读路径用的非抛出变体：规则不可读 → 空集（无人的终态操作被采纳，诚实降级） */
  private async writeSetOrEmpty(projectAffairId: string): Promise<ReadonlySet<string>> {
    try {
      return await this.writeSetOf(projectAffairId);
    } catch {
      return new Set();
    }
  }

  /** 从 PR 子事务创世记录定位 parent 项目议题（缺 parent → null，由调用方定口径） */
  private parentProjectOf(genesis: unknown): string | null {
    const refs = (genesis as Record<string, unknown> | null)?.refs;
    if (!Array.isArray(refs)) {
      return null;
    }
    const parent = refs.find(
      (ref) => typeof ref === 'object' && ref !== null && (ref as Record<string, unknown>).rel === 'parent'
        && typeof (ref as Record<string, unknown>).target === 'string'
    ) as Record<string, unknown> | undefined;
    return (parent?.target as string) ?? null;
  }

  /** 断言当前 actor ∈ 写权集合；不符即拒（伪造终态在提交前被拦下并留痕于错误消息） */
  private async requireWriteMembership(projectAffairId: string): Promise<void> {
    const actor = await this.ensureActor();
    let writeSet: ReadonlySet<string>;
    try {
      writeSet = await this.writeSetOf(projectAffairId);
    } catch (error) {
      throw new Error(`无法读取项目议题规则文档，写权校验 fail-closed 中止：${(error as Error).message}`);
    }
    if (!writeSet.has(actor.identity)) {
      const empty = writeSet.size === 0 ? '（规则文档未声明 maintainers，无人可执行终态操作）' : '';
      throw new Error(
        `终态操作被拒绝：当前身份 ${actor.identity.slice(0, 12)}… 不在项目议题规则声明的写权集合（rules.maintainers）中${empty}`
      );
    }
  }

  /** 读 PR 子事务时附带的写权集合（经创世 parent → 项目议题规则；失败回退空集） */
  private async writeSetForPrGenesis(genesis: unknown): Promise<ReadonlySet<string>> {
    const parent = this.parentProjectOf(genesis);
    return parent ? this.writeSetOrEmpty(parent) : new Set();
  }

  private async ensureCollections(): Promise<void> {
    if (this.collectionsReady) {
      return;
    }
    await this.sdk.data.declareCollection({ name: this.collections.bindings, merge: 'lww-record' });
    await this.sdk.data.declareCollection({ name: this.collections.knownPrs, merge: 'lww-record' });
    this.collectionsReady = true;
  }

  // ------------------------------------------------------------------
  // 项目议题绑定
  // ------------------------------------------------------------------

  async getBinding(): Promise<ProjectBinding | null> {
    await this.ensureCollections();
    const doc = await this.sdk.data.get<Record<string, unknown>>(this.collections.bindings, BINDING_DOC_ID);
    if (!doc || typeof doc.projectAffairId !== 'string' || !/^[0-9a-f]{64}$/.test(doc.projectAffairId)) {
      return null;
    }
    return { projectAffairId: doc.projectAffairId, repoName: typeof doc.repoName === 'string' ? doc.repoName : '' };
  }

  async bindProject(projectAffairId: string, repoName: string): Promise<void> {
    if (!/^[0-9a-f]{64}$/.test(projectAffairId)) {
      throw new Error('项目议题 affairId 形状非法（须为 64 位小写 hex）');
    }
    await this.ensureCollections();
    await this.sdk.data.save(this.collections.bindings, BINDING_DOC_ID, { projectAffairId, repoName });
  }

  /** 本机关注的事务清单（绑定选择器用；创世未同步的跳过不编造） */
  async listFollowedTopics(): Promise<Array<{ affairId: string; title: string; type: string }>> {
    const ids = await this.affairs.listFollowed();
    const items = await Promise.all(
      ids.map(async (affairId) => {
        const log = await this.affairs.readLog(affairId);
        const genesis = log.genesis as Record<string, unknown> | null;
        if (!genesis || typeof genesis.title !== 'string') {
          return null;
        }
        return { affairId, title: genesis.title, type: typeof genesis.type === 'string' ? genesis.type : '' };
      })
    );
    return items.filter(nonNull);
  }

  /** 关注已有事务（创世记录原文，affairId 自认证复算；非法记录由内核校验链拒绝） */
  followGenesis(genesis: unknown): Promise<string> {
    if (typeof genesis !== 'object' || genesis === null || Array.isArray(genesis)) {
      throw new Error('创世记录必须是 JSON 对象');
    }
    return this.affairs.follow(genesis as Record<string, unknown>);
  }

  subscribeChanges(handler: (event: { affairId: string; change: string }) => void): Promise<void> {
    return this.affairs.onChange(handler);
  }

  // ------------------------------------------------------------------
  // 镜像：清单读取 / 对象装载 / 浏览（纯 JS，桌面移动同码）
  // ------------------------------------------------------------------

  /** 读项目议题最新镜像清单 + 本地持有度（持有即做种副本健康如实呈现） */
  async getMirrorView(projectAffairId: string): Promise<MirrorView | null> {
    const log = await this.affairs.readLog(projectAffairId);
    const latest = latestMirrorManifest(log.ops);
    if (!latest) {
      return null;
    }
    const localCids = new Set(await this.content.listBlobs());
    return { manifest: latest.manifest, opHash: latest.opHash, conflicts: latest.conflicts, status: mirrorSyncStatus(latest.manifest, localCids) };
  }

  /**
   * 装载镜像对象为内存 store（浏览的数据来源）。
   * fetch=true 时缺失对象经 Kad provider 拉回（CID 校验由内核负责）；
   * 拉回失败的 cid 进 missing——UI 如实标注「对象暂不可拉取」，不伪造内容。
   */
  async loadObjectStore(
    manifest: MirrorManifest,
    options: { fetch: boolean } = { fetch: true }
  ): Promise<{ store: GitObjectStore; missing: string[]; loaded: number }> {
    const objects = new Map<string, GitObject>();
    const missing: string[] = [];
    for (const ref of manifest.objects) {
      const base64 = options.fetch ? await this.content.fetchBlob(ref.cid) : await this.content.readBlob(ref.cid);
      if (base64 === null) {
        missing.push(ref.cid);
        continue;
      }
      objects.set(ref.sha, { type: ref.type, bytes: base64ToBytes(base64) });
    }
    return { store: { get: (sha) => objects.get(sha) ?? null }, missing, loaded: objects.size };
  }

  /** 分支 head 的提交历史（缺对象的分支如实截断，不伪造连续历史） */
  listHistory(store: GitObjectStore, branchHead: string, limit = 200): ParsedCommit[] {
    return listCommits(store, branchHead, limit);
  }

  /** 提交详情的文件变更集（首个 parent 为基准；根提交对空树 diff） */
  commitChanges(store: GitObjectStore, commitSha: string): FileChange[] {
    const commit = readCommit(store, commitSha);
    if (!commit) {
      return [];
    }
    const baseTree = commit.parents.length > 0 ? readCommit(store, commit.parents[0])?.tree ?? null : null;
    return diffTrees(store, baseTree, commit.tree);
  }

  /** 目录 listing（path 为空 = 根树） */
  listTree(store: GitObjectStore, commitSha: string, path: string): TreeEntry[] | null {
    const commit = readCommit(store, commitSha);
    if (!commit) {
      return null;
    }
    if (path === '') {
      const tree = store.get(commit.tree);
      return tree && tree.type === 'tree' ? parseTree(tree.bytes) : null;
    }
    const entry = lookupPath(store, commit.tree, path);
    if (!entry || entry.type !== 'tree') {
      return null;
    }
    const tree = store.get(entry.sha);
    return tree && tree.type === 'tree' ? parseTree(tree.bytes) : null;
  }

  /** 读文件（二进制如实标注；文本按 UTF-8 解码） */
  readFile(
    store: GitObjectStore,
    commitSha: string,
    path: string
  ): { entry: TreeEntry; text: string | null; binary: boolean; size: number } | null {
    const commit = readCommit(store, commitSha);
    if (!commit) {
      return null;
    }
    const entry = lookupPath(store, commit.tree, path);
    if (!entry || entry.type !== 'blob') {
      return null;
    }
    const blob = store.get(entry.sha);
    if (!blob || blob.type !== 'blob') {
      return null;
    }
    const binary = looksBinary(blob.bytes);
    return { entry, text: binary ? null : bytesToUtf8(blob.bytes), binary, size: blob.bytes.length };
  }

  readBlobText(store: GitObjectStore, sha: string): { text: string | null; binary: boolean; size: number } | null {
    const blob = store.get(sha);
    if (!blob || blob.type !== 'blob') {
      return null;
    }
    const binary = looksBinary(blob.bytes);
    return { text: binary ? null : bytesToUtf8(blob.bytes), binary, size: blob.bytes.length };
  }

  // ------------------------------------------------------------------
  // 镜像发布（维护者，桌面）
  // ------------------------------------------------------------------

  /**
   * 发布镜像新版本：枚举发布分支**可达**的 git 对象（rev-list 口径——不可达对象、
   * stash、未发布私有 ref 不进入分发面）→ 逐对象 saveBlob（幂等，保存即声明
   * provider；sha 已在上一版清单中的对象复用其 cid，跳过重复 saveBlob/pinRoot）
   * → 项目议题提交 git.mirror.manifest 操作（version = 最新+1；首版记录
   * importHead = 当前默认分支 head，档一-4 一次性导入口径）。
   */
  async publishMirror(
    projectAffairId: string,
    input: { repoDir: string; repoName: string; note?: string; onProgress?: (done: number, total: number) => void }
  ): Promise<{ version: number; objectCount: number; opHash: string }> {
    const exec = this.requireExec();
    const existing = await this.getMirrorView(projectAffairId);
    const version = (existing?.manifest.version ?? 0) + 1;
    const branches = await listLocalBranches(exec, input.repoDir);
    if (branches.length === 0) {
      throw new Error('本地仓库没有任何分支（请先提交）');
    }
    const defaultBranch = existing?.manifest.defaultBranch ?? pickDefaultBranch(branches) ?? branches[0].name;
    const objects = await listReachableObjects(exec, input.repoDir, branches.map((branch) => branch.head));
    const previousCidBySha = new Map((existing?.manifest.objects ?? []).map((ref) => [ref.sha, ref.cid]));
    const refs: MirrorManifest['objects'] = [];
    const newCids: string[] = [];
    let done = 0;
    for (const obj of objects) {
      const knownCid = previousCidBySha.get(obj.sha);
      if (knownCid) {
        // 上一版清单已 pin 住（同一 mirror:{affairId} 根）——幂等跳过，增量发布
        refs.push({ sha: obj.sha, type: obj.type, cid: knownCid });
      } else {
        const base64 = await readObjectBase64(exec, input.repoDir, obj.sha, obj.type);
        const info = await this.content.saveBlob(base64);
        refs.push({ sha: obj.sha, type: obj.type, cid: info.cid });
        newCids.push(info.cid);
      }
      done += 1;
      input.onProgress?.(done, objects.length);
    }
    const manifest: MirrorManifest = {
      repo: input.repoName,
      defaultBranch,
      branches,
      version,
      objects: refs,
      ...(version === 1 ? { importHead: branches.find((b) => b.name === defaultBranch)?.head } : {}),
      ...(input.note?.trim() ? { note: input.note.trim() } : {})
    };
    const verdict = validateManifestInput(manifest);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    // pinRoot：新进入镜像的对象被新清单 pin 住（PR 附件之外的另一组 GC 根，§2 流程三）
    for (const cid of new Set(newCids)) {
      await this.content.pinRoot(cid, `mirror:${projectAffairId}`);
    }
    const { opHash } = await this.submitPayload(projectAffairId, buildMirrorManifestPayload(manifest));
    return { version, objectCount: refs.length, opHash };
  }

  // ------------------------------------------------------------------
  // PR：发现 / 详情 / 发起 / 修订 / 评审 / 合并 / 关闭
  // ------------------------------------------------------------------

  /**
   * 已知 PR 清单：本机关注的事务中筛 PR 类型 + parent 指向本项目（SDK 无
   * 子事务发现面——他机发起的 PR 经卡片/创世转发关注后才会出现，见报告缺口）。
   */
  async listKnownPrs(projectAffairId: string): Promise<PrSummaryView[]> {
    const followed = await this.affairs.listFollowed();
    const writeSet = await this.writeSetOrEmpty(projectAffairId);
    const views = await Promise.all(
      followed.map(async (affairId) => {
        if (affairId === projectAffairId) {
          return null;
        }
        const log = await this.affairs.readLog(affairId);
        const genesis = log.genesis as Record<string, unknown> | null;
        if (!genesis || genesis.type !== PR_AFFAIR_TYPE || !Array.isArray(genesis.refs)) {
          return null;
        }
        const isChild = genesis.refs.some(
          (ref) => typeof ref === 'object' && ref !== null && (ref as Record<string, unknown>).target === projectAffairId
            && (ref as Record<string, unknown>).rel === 'parent'
        );
        if (!isChild) {
          return null;
        }
        return { affairId, state: derivePrState(log.ops, writeSet) };
      })
    );
    return views.filter(nonNull);
  }

  async getPrDetail(prAffairId: string): Promise<PrState> {
    const log = await this.affairs.readLog(prAffairId);
    const writeSet = await this.writeSetForPrGenesis(log.genesis);
    return derivePrState(log.ops, writeSet);
  }

  private requireExec(): ExecFn {
    if (!this.execFn) {
      throw new Error('当前环境无桌面执行能力（sdk.sys 缺席或移动端）——写路径不可用；浏览不受影响。桌面端请安装并授权 system:exec。');
    }
    return this.execFn;
  }

  /** git CLI 状态（写路径入口的如实降级依据） */
  async gitCliStatus(): Promise<{ available: boolean; version: string | null }> {
    if (!this.execFn) {
      return { available: false, version: null };
    }
    return detectGitCli(this.execFn);
  }

  /**
   * 发起 PR（桌面）：本地仓库生成 bundle（base..branch）→ saveBlob 入内容面
   * （cid = SHA-256，保存即声明 provider）→ sdk.affairs.create 建 PR 子事务
   * （refs parent → 项目议题）→ pr.open 操作入日志 → 附件 pinRoot(pr:{affairId})
   * → 应用会话卡片通知（降级不阻断）。
   */
  async openPr(input: {
    projectAffairId: string;
    repoDir: string;
    branch: string;
    base: string;
    title: string;
    description: string;
  }): Promise<{ prAffairId: string; cid: string; size: number; opHash: string }> {
    const exec = this.requireExec();
    const mirror = await this.getMirrorView(input.projectAffairId);
    if (!mirror) {
      throw new Error('项目议题尚无镜像清单（请先由维护者发布镜像）');
    }
    const baseBranch = mirror.manifest.branches.find((branch) => branch.name === input.base);
    if (!baseBranch) {
      throw new Error(`镜像清单中不存在 base 分支 ${input.base}`);
    }
    const head = await resolveRef(exec, input.repoDir, input.branch);
    const ancestor = await isAncestorCli(exec, input.repoDir, baseBranch.head, head);
    if (!ancestor) {
      throw new Error(`分支 ${input.branch} 不以镜像 ${input.base} head 为祖先——请先 rebase/合并最新镜像再发起 PR`);
    }
    if (baseBranch.head === head) {
      throw new Error('分支相对 base 没有新提交，无可评审改动');
    }
    const bundlePath = `${input.repoDir}/.git/spark-pr-${Date.now()}.bundle`;
    const bundleBase64 = await createBundleBase64(exec, input.repoDir, baseBranch.head, input.branch, bundlePath);
    const blob = await this.content.saveBlob(bundleBase64);
    const attachment: PrAttachment = {
      kind: 'bundle',
      cid: blob.cid,
      size: blob.size,
      name: `${input.branch.replace(/[^\w.-]/g, '_')}.bundle`
    };
    const verdict = validatePrOpenInput({
      title: input.title,
      description: input.description,
      base: input.base,
      head,
      attachments: [attachment]
    });
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    const genesisInput: AffairGenesisInput = {
      type: PR_AFFAIR_TYPE,
      title: input.title.trim(),
      summary: input.description.trim().slice(0, 200) || input.title.trim(),
      tags: ['pr'],
      refs: [buildParentRef(input.projectAffairId)],
      rules: {
        engine: 'b1',
        closeConditions: [],
        pubPeriod: { delayMs: 86_400_000, vetoThreshold: { count: 1 } },
        ruleChange: { kind: 'delayed-veto', delayMs: 86_400_000, vetoThreshold: { count: 1 } },
        exec: null
      },
      extra: {}
    };
    const { affairId } = await this.affairs.create(genesisInput);
    await this.content.pinRoot(blob.cid, `pr:${affairId}`);
    const { opHash } = await this.submitPayload(
      affairId,
      buildPrOpenPayload({
        title: input.title,
        description: input.description,
        base: input.base,
        head,
        attachments: [attachment]
      })
    );
    await this.registerKnownPr(affairId, input.projectAffairId);
    return { prAffairId: affairId, cid: blob.cid, size: blob.size, opHash };
  }

  /** 修订 PR（同一子事务追加 pr.update：新 bundle cid + 新 head，append-only 可考） */
  async updatePr(input: {
    prAffairId: string;
    projectAffairId: string;
    repoDir: string;
    branch: string;
    note?: string;
  }): Promise<{ cid: string; head: string; opHash: string }> {
    const exec = this.requireExec();
    const state = await this.getPrDetail(input.prAffairId);
    if (state.status !== 'open' || !state.open) {
      throw new Error('PR 不在开放状态，无法修订');
    }
    const mirror = await this.getMirrorView(input.projectAffairId);
    const baseHead = mirror?.manifest.branches.find((branch) => branch.name === state.open?.base)?.head;
    if (!baseHead) {
      throw new Error('镜像清单中找不到 PR 的 base 分支 head（等待镜像同步或联系维护者）');
    }
    const head = await resolveRef(exec, input.repoDir, input.branch);
    if (head === state.currentHead) {
      throw new Error('分支 head 未变化，无修订内容');
    }
    // 与 openPr 同口径的祖先校验（坏修订在提交前暴露，而非拖到维护者合并时）
    if (!(await isAncestorCli(exec, input.repoDir, baseHead, head))) {
      throw new Error(`分支 ${input.branch} 不以镜像 ${state.open.base} head 为祖先——请先 rebase/合并最新镜像再修订`);
    }
    const bundlePath = `${input.repoDir}/.git/spark-pr-${Date.now()}.bundle`;
    const bundleBase64 = await createBundleBase64(exec, input.repoDir, baseHead, input.branch, bundlePath);
    const blob = await this.content.saveBlob(bundleBase64);
    await this.content.pinRoot(blob.cid, `pr:${input.prAffairId}`);
    const attachment: PrAttachment = { kind: 'bundle', cid: blob.cid, size: blob.size, name: `${input.branch.replace(/[^\w.-]/g, '_')}-r${state.updates.length + 2}.bundle` };
    const { opHash } = await this.submitPayload(
      input.prAffairId,
      buildPrUpdatePayload({ head, attachments: [attachment], ...(input.note?.trim() ? { note: input.note.trim() } : {}) })
    );
    return { cid: blob.cid, head, opHash };
  }

  /** 评论（评审参与者；ref 可选锚定文件+行号，行内呈现排后续） */
  async commentPr(prAffairId: string, text: string, ref?: { path: string; line: number }): Promise<{ opHash: string }> {
    if (text.trim().length < 2) {
      throw new Error('评论内容过短');
    }
    return this.submitPayload(prAffairId, buildPrCommentPayload({ text: text.trim(), ...(ref ? { ref } : {}) }));
  }

  /** 评审表态（approve / request-changes / comment） */
  async reviewPr(prAffairId: string, verdict: PrReviewVerdict, text: string): Promise<{ opHash: string }> {
    return this.submitPayload(prAffairId, buildPrReviewPayload({ verdict, text: text.trim() }));
  }

  /**
   * 维护者合并（桌面；档一-3：单维护者合并回执即生效 + 禁止非快进写回）：
   * 写权校验（actor ∈ 议题规则 maintainers，不符即拒）→ 拉回附件（全部 provider
   * 不可达 → 如实报错「附件暂不可拉取」）→ 本地 fetch bundle → 合并（先
   * --ff-only，退化三方合并；冲突如实上抛不代决）→ FF 复核 → 清理临时
   * refs/spark-pr/* → 发布新镜像版本 → pr.merged 回执（resultCommit + 新镜像
   * 版本指针）→ 附件解除 pinRoot（进入宽限回收，不再做种）。
   */
  async mergePr(input: {
    prAffairId: string;
    projectAffairId: string;
    repoDir: string;
    note?: string;
    onProgress?: (stage: string) => void;
  }): Promise<{ resultCommit: string; mirrorVersion: number; opHash: string }> {
    const exec = this.requireExec();
    const progress = input.onProgress ?? (() => undefined);
    const state = await this.getPrDetail(input.prAffairId);
    if (state.status !== 'open' || !state.open) {
      throw new Error('PR 不在开放状态（已合并或已关闭）');
    }
    // 终态操作写权校验（档一-3）：伪造 merged 在提交前被拦下
    await this.requireWriteMembership(input.projectAffairId);
    const attachment = state.currentAttachments[0];
    if (!attachment) {
      throw new Error('PR 没有可用附件');
    }
    progress('拉取附件');
    const bundleBase64 = await this.content.fetchBlob(attachment.cid);
    if (bundleBase64 === null) {
      throw new Error('附件暂不可拉取（所有 provider 不可达）——请稍后重试，PR 状态未变');
    }
    const mirror = await this.getMirrorView(input.projectAffairId);
    if (!mirror) {
      throw new Error('项目议题尚无镜像清单');
    }
    const baseBranch = mirror.manifest.branches.find((branch) => branch.name === state.open?.base);
    if (!baseBranch) {
      throw new Error(`镜像清单中不存在 base 分支 ${state.open.base}`);
    }
    progress('写入 bundle 并取回提交');
    const bundlePath = `${input.repoDir}/.git/spark-merge-${Date.now()}.bundle`;
    await writeFileBase64(exec, bundlePath, bundleBase64);
    const prHead = state.currentHead;
    if (!prHead) {
      await removeFile(exec, bundlePath);
      throw new Error('PR 缺少 head commit（元数据不全，等待复制收敛）');
    }
    const localRef = `refs/spark-pr/${input.prAffairId.slice(0, 12)}`;
    let resultCommit: string;
    try {
      await fetchBundle(exec, input.repoDir, bundlePath, prHead, localRef);
      progress('执行合并');
      await checkoutBranch(exec, input.repoDir, baseBranch.name);
      const before = await resolveRef(exec, input.repoDir, baseBranch.name);
      if (before !== baseBranch.head) {
        throw new Error(
          `本地 ${baseBranch.name}（${before.slice(0, 12)}…）与镜像清单 head（${baseBranch.head.slice(0, 12)}…）不一致——请先把权威仓库同步到最新镜像再合并（串行合并约定）`
        );
      }
      try {
        resultCommit = await mergeFfOnly(exec, input.repoDir, localRef);
      } catch {
        // 非线性历史：退化为三方合并（合并提交本身以 base 为父，仍满足 FF 写回规则）
        resultCommit = await mergeCommit(exec, input.repoDir, localRef, `Merge PR ${input.prAffairId.slice(0, 12)} (${state.open.title})`);
      }
      progress('快进校验');
      if (!(await isAncestorCli(exec, input.repoDir, before, resultCommit))) {
        throw new Error('非快进写回被拒绝（档一-3）：合并结果不以当前镜像 head 为祖先——已中止，未发布任何变更');
      }
    } finally {
      await removeFile(exec, bundlePath);
    }
    // 清理本地临时 PR ref（逐次累积会污染本地 ref 空间；删除失败不阻断主流程）
    try {
      await deleteRef(exec, input.repoDir, localRef);
    } catch {
      // 尽力而为
    }
    progress('发布新镜像版本');
    const published = await this.publishMirror(input.projectAffairId, {
      repoDir: input.repoDir,
      repoName: mirror.manifest.repo,
      note: input.note?.trim() ? input.note.trim() : `合并 PR ${input.prAffairId.slice(0, 12)}`
    });
    progress('提交合并回执');
    const { opHash } = await this.submitPayload(
      input.prAffairId,
      buildPrMergedPayload({
        resultCommit,
        mirrorVersion: published.version,
        ...(input.note?.trim() ? { note: input.note.trim() } : {})
      })
    );
    await this.unpinAttachments(input.prAffairId, state);
    return { resultCommit, mirrorVersion: published.version, opHash };
  }

  /** 关闭 PR（维护者；写权校验 + 理由入日志）→ 附件解除 pinRoot */
  async closePr(prAffairId: string, reason: string): Promise<{ opHash: string }> {
    const log = await this.affairs.readLog(prAffairId);
    const projectAffairId = this.parentProjectOf(log.genesis);
    if (!projectAffairId) {
      throw new Error('PR 子事务创世缺少 parent 引用——无法定位项目议题做写权校验，关闭被拒绝（fail-closed）');
    }
    const writeSet = await this.writeSetOrEmpty(projectAffairId);
    const state = derivePrState(log.ops, writeSet);
    if (state.status !== 'open') {
      throw new Error('PR 已处于终态');
    }
    if (reason.trim().length < 2) {
      throw new Error('请填写关闭理由');
    }
    // 终态操作写权校验（档一-3）：伪造 closed 在提交前被拦下
    await this.requireWriteMembership(projectAffairId);
    const result = await this.submitPayload(prAffairId, buildPrClosedPayload({ reason: reason.trim() }));
    await this.unpinAttachments(prAffairId, state);
    return result;
  }

  /** 附件 pinRoot(pr:{affairId}) 全部解除（PR 关闭后进入宽限回收，不再做种，§2 流程三） */
  private async unpinAttachments(prAffairId: string, state: PrState): Promise<void> {
    const seen = new Set<string>();
    const attachments = [...(state.open?.attachments ?? []), ...state.updates.flatMap((update) => update.attachments)];
    for (const attachment of attachments) {
      if (seen.has(attachment.cid)) {
        continue;
      }
      seen.add(attachment.cid);
      try {
        await this.content.unpinRoot(attachment.cid, `pr:${prAffairId}`);
      } catch {
        // 解除失败不阻断关闭流程（无根判定以内核为准，宽限期后仍走两段式回收）
      }
    }
  }

  /**
   * 合并回执独立核验（§3.4）：回执 commit 须出现在回执所指镜像版本的历史中，
   * 且须包含 PR head（否则维护者可合入无关提交开回执）。
   * 返回 null = 回执所指版本清单未同步到本机（如实标注，不伪造通过）。
   */
  async verifyMergedPr(
    projectAffairId: string,
    state: PrState
  ): Promise<{ ok: boolean; checkedBranch: string | null; reason: string | null } | null> {
    if (!state.merged) {
      return null;
    }
    const log = await this.affairs.readLog(projectAffairId);
    const target = this.findManifestVersion(log.ops, state.merged.mirrorVersion);
    if (!target) {
      return null;
    }
    const { store } = await this.loadObjectStore(target, { fetch: true });
    return verifyMergeReceipt(store, target, state.merged, state.currentHead ?? undefined);
  }

  /** 在议题日志中定位指定版本的镜像清单（同号取 opHash 大者，与 latestMirrorManifest 同口径） */
  private findManifestVersion(ops: AffairLogEntry[], version: number): MirrorManifest | null {
    let best: { manifest: MirrorManifest; opHash: string } | null = null;
    for (const entry of ops) {
      const op = entry.op as Record<string, unknown>;
      const parsed = parseMirrorManifest(op?.payload);
      if (!parsed || parsed.version !== version) {
        continue;
      }
      if (!best || entry.opHash > best.opHash) {
        best = { manifest: parsed, opHash: entry.opHash };
      }
    }
    return best?.manifest ?? null;
  }

  // ------------------------------------------------------------------
  // 物化工作区（桌面 clone；只读镜像 → 本地 git 仓库）
  // ------------------------------------------------------------------

  /**
   * 一键物化：镜像对象（缺失先经 Kad 拉回）→ 本地 git init + hash-object 重建
   * 对象库 + 分支 + 检出工作区。物化出的本地仓库是完全的 git 仓库，本地随意
   * 提交与 Spark 无关（§2 流程一）。
   */
  async materialize(
    projectAffairId: string,
    targetDir: string,
    onProgress?: (done: number, total: number) => void
  ): Promise<{ objectCount: number; defaultBranch: string }> {
    const exec = this.requireExec();
    const mirror = await this.getMirrorView(projectAffairId);
    if (!mirror) {
      throw new Error('项目议题尚无镜像清单');
    }
    const { store, missing } = await this.loadObjectStore(mirror.manifest, { fetch: true });
    if (missing.length > 0) {
      throw new Error(`有 ${missing.length} 个镜像对象暂不可拉取（provider 不可达）——物化中止，请稍后重试`);
    }
    const objects = mirror.manifest.objects.map((ref) => {
      const obj = store.get(ref.sha);
      if (!obj) {
        throw new Error(`镜像对象 ${ref.sha.slice(0, 12)}… 装载失败`);
      }
      return { sha: ref.sha, type: ref.type, base64: bytesToBase64(obj.bytes) };
    });
    await materializeRepo(exec, targetDir, objects, mirror.manifest.branches, mirror.manifest.defaultBranch, onProgress);
    return { objectCount: objects.length, defaultBranch: mirror.manifest.defaultBranch };
  }

  // ------------------------------------------------------------------
  // 通知与登记
  // ------------------------------------------------------------------

  /**
   * PR 事件的应用会话卡片通知（message:app；「消息自描述」——summary 为未装
   * 插件成员可见的纯文本摘要）。权限被拒/限流降级返回 false，不阻断主流程。
   */
  async notifyPrCard(input: { affairId: string; title: string; status: string; base: string }): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        { summary: `[PR] ${input.title}（base: ${input.base}）— ${input.status}`, affairId: input.affairId },
        { viewId: 'pr-card', data: { affairId: input.affairId, title: input.title, status: input.status, base: input.base } }
      );
      return true;
    } catch (error) {
      console.warn('[spark-git-repo] PR 卡片通知失败（权限/限流降级）：', error);
      return false;
    }
  }

  private async registerKnownPr(prAffairId: string, projectAffairId: string): Promise<void> {
    await this.ensureCollections();
    await this.sdk.data.save(this.collections.knownPrs, prAffairId, { prAffairId, projectAffairId, seenAt: Date.now() });
  }
}
