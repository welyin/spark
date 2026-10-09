/**
 * 发布管理插件（spark-release-manager）· 数据模型与纯函数。
 *
 * 设计依据：wiki/product/bootstrap-plugins/release-management.md v0.2 §3（数据模型）
 * + decisions.md 拍板口径。
 *
 * 边界红线（§1）：构建在 CI（本插件不构建）；签名验签、包哈希进存证链、P2P 分发
 * 是内核/协议能力（本插件不重新实现）——本插件只做产品面：登记（发布单入
 * governance append-only 集合，包哈希随之天然入存证链）、编排（事件流派生状态机）、
 * 跟踪（渠道与版本分布可见性）。只验不签发布包、不持有私钥。
 *
 * 拍板落点：
 * - 档一-2：MVP 版本卡片由本插件唯一推送，releaseRef（= 发布单记录 id，档三-24）
 *   /版本号为幂等键，经 sdk.messages 本机生成（与 spark-announcement 同范式）；
 * - 档一-6：核验 MVP = 本机导入复算（sdk.market.inspectLocal 内核复算整包
 *   sha256/size + 与发布单登记值/update-manifest 资产三方比对）；验签信任链沿用
 *   内核市场通路（本插件不重实现，证据如实标注）；在线抓取复算非验收要件；
 * - 档二-8：在线抓取复算（network:fetch，桌面限定）排后续迭代——MVP 不声明
 *   network:fetch（未使用的高危权限不挂清单，授权层无平台门控），移动端降级
 *   只做登记；
 * - 档三-23：发布权集合管理员直改初始化；档三-25：版本上报 MVP 只做集合与视图
 *   骨架（如实标注非全量）；档三-26：撤回 = 组织内登记 + 公告告知，不回滚已安装。
 *
 * 本文件不依赖 SDK 运行时/Vue，全部可单测。
 */

/** 版本号上限（semver 形态校验 + 长度卫生边界） */
export const RELEASE_MAX_VERSION_LENGTH = 40;
/** 目标插件引用（仓库地址形态）上限 */
export const RELEASE_MAX_PLUGIN_REF_LENGTH = 200;
/** 单发布单资产条数上限 */
export const RELEASE_MAX_ARTIFACTS = 32;
/** 文件名 / 来源 URL 上限 */
export const RELEASE_MAX_FILE_NAME_LENGTH = 200;
export const RELEASE_MAX_URL_LENGTH = 500;
/** 变更说明上限（MVP 纯文本；长文走内容面 blob changelogRef 排后续） */
export const RELEASE_MAX_CHANGELOG_LENGTH = 20000;
/** 关联事务引用上限 */
export const RELEASE_MAX_DECISION_REF_LENGTH = 120;
/** 渠道目标 / 备注上限 */
export const RELEASE_MAX_CHANNEL_TARGET_LENGTH = 300;
export const RELEASE_MAX_CHANNEL_NOTE_LENGTH = 500;
/** 事件详情 / 失败原因 / 撤回理由上限 */
export const RELEASE_MAX_EVENT_DETAIL_LENGTH = 2000;
export const RELEASE_MAX_REASON_LENGTH = 500;
/** 发布者集合人数上限（卫生边界） */
export const RELEASE_MAX_PUBLISHERS = 100;
/** 应用消息 summary 硬上限（内核 APP_SUMMARY_MAX_CHARS） */
export const RELEASE_SUMMARY_LIMIT = 200;
/** 版本卡片补发阈值（同公告插件限流预算口径：超出只补最新一条 + 汇总消息） */
export const RELEASE_BACKFILL_FULL_THRESHOLD = 5;

// ------------------------------------------------------------------
// 发布单（releases，governance append-only + sync；§3 数据模型）
// ------------------------------------------------------------------

/**
 * 资产条目（CI 既定产物集：.spkg / update-manifest.json / .sig / .pub.pem /
 * checksums，plugin-release.md）。kind 保持开放字符串（CI 产物线形可能扩展，
 * 如 sbom）——登记值不是新的信任源，而是把构建侧承诺钉进存证链供事后比对。
 */
export type ReleaseArtifact = {
  kind: string;
  fileName: string;
  /** sha256，64 位小写 hex */
  sha256: string;
  size: number;
  url?: string;
};

/**
 * 发布单记录。governance 语义集合（强制 append-only + 链式存证）：发布单内容
 * （含包哈希）随条目入存证链——「包哈希进存证链」的产品侧落点，无需插件自建
 * 哈希链。发布单不可编辑：状态推进走 release_events 事件流（状态是派生量）。
 */
export type ReleaseRecord = {
  id: string;
  orgId: string;
  /** 目标插件（plugin-dist §1：仓库地址形态，名字可抢注 URL 不可抢注） */
  pluginId: string;
  /**  semver 形态版本号 */
  version: string;
  artifacts: ReleaseArtifact[];
  /** update-manifest.json 解析原文（登记向导导入；核验时与登记资产双向比对） */
  updateManifest?: UpdateManifest;
  /** 变更说明（MVP 纯文本内联；长文走 sdk.content blob changelogRef 排后续） */
  changelog?: string;
  /** 变更说明长文的内容面 cid（内容面引用，发布单本体保持小体量） */
  changelogRef?: string;
  /** 关联事务（发布决议 / 里程碑议题 id，只存引用不读对方数据） */
  decisionRef?: string;
  /** 目标渠道 id 列表（channels 集合声明性登记） */
  channels: string[];
  publisherRootId: string;
  createdAt: number;
  signature?: ReleaseSignature;
};

/** CI 产出的 update-manifest.json 线形（plugins/scripts/build-plugin-package.mjs） */
export type UpdateManifest = {
  pluginId?: string;
  domain?: string;
  manifestVersion?: number;
  version?: string;
  releaseTime?: string;
  assets?: Array<{
    kind?: string;
    fileName?: string;
    url?: string;
    sha256?: string;
    size?: number;
  }>;
};

// ------------------------------------------------------------------
// 状态事件（release_events，governance append-only + sync）
// ------------------------------------------------------------------

/**
 * 状态事件类型（§3）：状态机由事件流派生，不改原发布单。
 * verified（核验通过）/ published（签名发布推进）/ channel-pushed（渠道推送登记）/
 * verify-failed（核验失败，原因原样）/ retracted（撤回，append-only 不回滚）。
 */
export type ReleaseEventType = 'verified' | 'published' | 'channel-pushed' | 'verify-failed' | 'retracted';

/** 本机导入复算的核验证据（随 verified / verify-failed 事件入链，逐项如实） */
export type ReleaseVerificationDetail = {
  /** 导入的 .spkg 文件名 */
  fileName?: string;
  /** 内核复算结果（sdk.market.inspectLocal：容器解析 + 整包 sha256/size） */
  recomputed?: { sha256: string; size: number; pluginId: string; version: string };
  /** 已比对的 update-manifest 资产条数 */
  manifestAssetsChecked: number;
  /** 逐项不一致原因原文（空数组 = 全部一致） */
  mismatches: string[];
  /** 签名材料（sig/pubkey 资产）是否已随发布单声明 */
  sigMaterialDeclared: boolean;
  /**
   * 信任链诚实标注（档一-6）：包签名验签在内核市场通路（内置目录签名链路 /
   * 仓库锚定安装）执行，本插件只验不签、不重实现验签——本字段固定说明这一点，
   * 不伪造「已验签」。
   */
  trustNote: string;
};

/** 状态事件记录（append-only；operator 签名防抵赖） */
export type ReleaseEvent = {
  id: string;
  orgId: string;
  releaseId: string;
  type: ReleaseEventType;
  /** 渠道推送登记的渠道 id / 结果引用 */
  channelId?: string;
  resultRef?: string;
  /** verify-failed / retracted 的原因原文（不替发布者遮掩） */
  reason?: string;
  /** 事件说明（渠道备注等） */
  detail?: string;
  /** 核验证据（verified / verify-failed 事件携带） */
  verification?: ReleaseVerificationDetail;
  operatorRootId: string;
  at: number;
  signature?: ReleaseSignature;
};

/** 发布单派生状态（§4 列表五态；派生量，不落库） */
export type ReleaseState = 'registered' | 'verified' | 'published' | 'verify-failed' | 'retracted';

export const RELEASE_STATE_LABELS: Record<ReleaseState, string> = {
  registered: '已登记',
  verified: '核验通过',
  published: '已发布',
  'verify-failed': '核验失败',
  retracted: '已撤回'
};

// ------------------------------------------------------------------
// 渠道（channels，lww-record + sync；声明性登记，不携带执行凭据）
// ------------------------------------------------------------------

/**
 * 渠道类型（§3）：内置目录更新（验收③走内置目录签名链路，档二-11）/
 * 仓库锚定声明文件 bump / 广播发布声明（gossipsub /spark/plugin-announce）/
 * .spkg 侧载包归档。
 */
export type ReleaseChannelKind = 'market-catalog' | 'repo-anchored' | 'announce' | 'sideload';

export const RELEASE_CHANNEL_KINDS: ReleaseChannelKind[] = ['market-catalog', 'repo-anchored', 'announce', 'sideload'];

export const RELEASE_CHANNEL_KIND_LABELS: Record<ReleaseChannelKind, string> = {
  'market-catalog': '内置目录',
  'repo-anchored': '仓库锚定',
  announce: '广播声明',
  sideload: '侧载归档'
};

export type ReleaseChannel = {
  id: string;
  orgId: string;
  kind: ReleaseChannelKind;
  /** 渠道目标（目录条目 / 声明文件地址 / announce topic / 归档位置） */
  target: string;
  note?: string;
  createdBy: string;
  createdAt: number;
  updatedAt: number;
};

// ------------------------------------------------------------------
// 版本上报（version_reports，append-only + sync；档三-25 MVP 骨架）
// ------------------------------------------------------------------

/** 安装信任级（内核市场口径，原样展示不美化） */
export type ReleaseTrustLevel = 'signed' | 'repo-anchored' | 'sideloaded' | 'builtin';

/**
 * 版本上报记录（成员 opt-in；最小字段，不含设备指纹）。
 * append-only：分布视图只显示计数分布，如实标注「仅含 opt-in 上报成员，非全量」。
 */
export type VersionReport = {
  id: string;
  orgId: string;
  pluginId: string;
  version: string;
  trust: string;
  reporterRootId: string;
  at: number;
};

// ------------------------------------------------------------------
// 发布权配置（release_config，lww；档三-23 管理员直改初始化）
// ------------------------------------------------------------------

export type ReleaseManagerConfig = {
  orgId: string;
  /** 发布权集合：域身份 rootId 列表（业务层校验，内核不认识「谁能发版」） */
  publisherRootIds: string[];
  createdBy: string;
  createdAt: number;
  updatedAt: number;
};

// ------------------------------------------------------------------
// 签名（identity:sign 防抵赖，与 spark-forum/spark-kanban 同范式）
// ------------------------------------------------------------------

export type ReleaseSignature = {
  /** 被签名的原文（buildReleaseSignPayload 产物）；验签侧从记录当前字段重算比对 */
  payload: string;
  signature: string;
  publicKey: string;
};

// ------------------------------------------------------------------
// 规范化与校验
// ------------------------------------------------------------------

export function normalizeReleaseText(text: string): string {
  return text.trim();
}

/** semver 形态（允许预发布/构建元数据后缀；不做语义比较，比较见 compareVersions） */
export const SEMVER_PATTERN = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

export function validateVersion(version: string): { ok: boolean; reason?: string } {
  const normalized = normalizeReleaseText(version);
  if (!normalized) {
    return { ok: false, reason: '版本号不能为空' };
  }
  if (normalized.length > RELEASE_MAX_VERSION_LENGTH) {
    return { ok: false, reason: `版本号长度不能超过${RELEASE_MAX_VERSION_LENGTH}字` };
  }
  if (!SEMVER_PATTERN.test(normalized)) {
    return { ok: false, reason: `版本号「${normalized}」不是合法 semver（形如 1.2.3 或 1.2.3-rc.1）` };
  }
  return { ok: true };
}

/**
 * 目标插件引用校验（plugin-dist §1.2 规范化在登记侧只做保守收口：trim +
 * 非空 + 长度；完整规范化随内核分发规格落地，登记原样入链供比对）。
 */
export function validatePluginRef(pluginId: string): { ok: boolean; reason?: string } {
  const normalized = normalizeReleaseText(pluginId);
  if (!normalized) {
    return { ok: false, reason: '目标插件引用不能为空' };
  }
  if (normalized.length > RELEASE_MAX_PLUGIN_REF_LENGTH) {
    return { ok: false, reason: `目标插件引用长度不能超过${RELEASE_MAX_PLUGIN_REF_LENGTH}字` };
  }
  return { ok: true };
}

export const SHA256_PATTERN = /^[0-9a-f]{64}$/;

/**
 * 资产清单校验：恰好一条 kind='package' 资产 + fileName 全清单唯一；哈希/size/
 * 文件名形态全量把关。包资产唯一性是核验语义的前提：复算比对以「该包的
 * fileName → 登记哈希」逐资产进行，重复 fileName / 多条 package 会让其中一条
 * 永不参与比对（登记值钉进存证链的哈希出现未验证豁口），登记侧直接拒绝。
 */
export function validateArtifacts(artifacts: ReleaseArtifact[]): { ok: boolean; reason?: string } {
  if (artifacts.length === 0) {
    return { ok: false, reason: '资产清单不能为空（至少登记 .spkg 包资产）' };
  }
  if (artifacts.length > RELEASE_MAX_ARTIFACTS) {
    return { ok: false, reason: `资产条数不能超过${RELEASE_MAX_ARTIFACTS}` };
  }
  const packageCount = artifacts.filter((asset) => asset.kind === 'package').length;
  if (packageCount === 0) {
    return { ok: false, reason: '资产清单必须包含一条 kind=package 的 .spkg 包资产' };
  }
  if (packageCount > 1) {
    return { ok: false, reason: 'kind=package 的包资产必须恰好一条（一包一发布单，多包请拆多条发布单登记）' };
  }
  const seenFileNames = new Set<string>();
  for (const asset of artifacts) {
    if (!normalizeReleaseText(asset.kind)) {
      return { ok: false, reason: '资产 kind 不能为空' };
    }
    if (!normalizeReleaseText(asset.fileName)) {
      return { ok: false, reason: '资产 fileName 不能为空' };
    }
    const fileName = normalizeReleaseText(asset.fileName);
    if (seenFileNames.has(fileName)) {
      return { ok: false, reason: `资产 fileName 重复：「${fileName}」（同名资产会让其中一条不参与复算比对）` };
    }
    seenFileNames.add(fileName);
    if (asset.fileName.length > RELEASE_MAX_FILE_NAME_LENGTH) {
      return { ok: false, reason: `资产文件名长度不能超过${RELEASE_MAX_FILE_NAME_LENGTH}字` };
    }
    if (!SHA256_PATTERN.test(asset.sha256)) {
      return { ok: false, reason: `资产「${asset.fileName}」的 sha256 必须是 64 位小写 hex` };
    }
    if (!Number.isSafeInteger(asset.size) || asset.size <= 0) {
      return { ok: false, reason: `资产「${asset.fileName}」的 size 必须是正整数` };
    }
    if (asset.url !== undefined && asset.url.length > RELEASE_MAX_URL_LENGTH) {
      return { ok: false, reason: `资产「${asset.fileName}」的来源 URL 过长` };
    }
  }
  return { ok: true };
}

export function validateChangelog(changelog: string | undefined): { ok: boolean; reason?: string } {
  if (changelog && normalizeReleaseText(changelog).length > RELEASE_MAX_CHANGELOG_LENGTH) {
    return { ok: false, reason: `变更说明长度不能超过${RELEASE_MAX_CHANGELOG_LENGTH}字` };
  }
  return { ok: true };
}

export function validateReason(reason: string | undefined, label: string): { ok: boolean; reason?: string } {
  if (reason && normalizeReleaseText(reason).length > RELEASE_MAX_REASON_LENGTH) {
    return { ok: false, reason: `${label}长度不能超过${RELEASE_MAX_REASON_LENGTH}字` };
  }
  return { ok: true };
}

// ------------------------------------------------------------------
// update-manifest 解析（登记向导：导入 CI 产出的 update-manifest.json）
// ------------------------------------------------------------------

/**
 * 解析 CI 产出的 update-manifest.json（plugins/scripts/build-plugin-package.mjs
 * 线形：{ pluginId, version, assets: [{kind, fileName, url, sha256, size}] }）。
 * 解析失败 / 线形缺失 → 抛出带原文原因的错误（视图层原样展示，不替发布者遮掩）。
 */
export function parseUpdateManifest(raw: string): { manifest: UpdateManifest; artifacts: ReleaseArtifact[] } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`update-manifest.json 不是合法 JSON：${(error as Error).message}`);
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error('update-manifest.json 必须是 JSON 对象');
  }
  const manifest = parsed as UpdateManifest;
  if (!Array.isArray(manifest.assets) || manifest.assets.length === 0) {
    throw new Error('update-manifest.json 缺少 assets 资产清单');
  }
  const artifacts: ReleaseArtifact[] = manifest.assets.map((asset, index) => {
    const fileName = normalizeReleaseText(asset.fileName ?? '');
    if (!fileName) {
      throw new Error(`update-manifest.json 第 ${index + 1} 条资产缺少 fileName`);
    }
    // kind 缺省不静默回落 'package'（多 package 资产伪造面）：仅 .spkg 可按
    // 扩展名推断为 package，其余缺 kind 一律报错，由发布者显式声明。
    let kind = normalizeReleaseText(asset.kind ?? '');
    if (!kind) {
      if (fileName.toLowerCase().endsWith('.spkg')) {
        kind = 'package';
      } else {
        throw new Error(`update-manifest.json 资产「${fileName}」缺少 kind 声明（仅 .spkg 可缺省推断为 package）`);
      }
    }
    const sha256 = normalizeReleaseText(asset.sha256 ?? '').toLowerCase();
    const size = typeof asset.size === 'number' ? asset.size : NaN;
    if (!SHA256_PATTERN.test(sha256)) {
      throw new Error(`update-manifest.json 资产「${fileName}」的 sha256 非法（须 64 位 hex）`);
    }
    if (!Number.isSafeInteger(size) || size <= 0) {
      throw new Error(`update-manifest.json 资产「${fileName}」的 size 非法（须正整数）`);
    }
    const url = normalizeReleaseText(asset.url ?? '');
    return url ? { kind, fileName, sha256, size, url } : { kind, fileName, sha256, size };
  });
  return { manifest, artifacts };
}

// ------------------------------------------------------------------
// 核验比对（本机导入复算，档一-6；纯函数：登记值 × update-manifest × 内核复算值）
// ------------------------------------------------------------------

/** 内核复算输入（sdk.market.inspectLocal 出参的取用子集） */
export type RecomputedPackage = {
  sha256: string;
  size: number;
  pluginId: string;
  version: string;
};

/**
 * 三方比对（plugin-dist §5 同一校验口径）：
 * 1. 发布单登记的 package 资产 sha256/size ↔ 内核复算值；
 * 2. update-manifest.json 内 assets ↔ 发布单登记资产（双重核对：两处不一致即失败——
 *    登记值是把构建侧承诺钉进存证链供事后比对）；
 * 3. update-manifest 声明的 pluginId/version ↔ 发布单 pluginId/version ↔ 容器内
 *    manifest 解析值（张冠李戴防护）。
 * 返回逐项不一致原因原文（空数组 = 全部一致）；原因原样入 verify-failed 事件。
 */
export function compareReleasePackage(
  release: Pick<ReleaseRecord, 'pluginId' | 'version' | 'artifacts' | 'updateManifest'>,
  recomputed: RecomputedPackage
): string[] {
  const mismatches: string[] = [];
  // 逐资产比对（不只第一条）：登记侧 validateArtifacts 已强制恰好一条 package，
  // 此处仍全量遍历——历史/旁路写入的发布单若含多条 package，每条登记哈希都必须
  // 与内核复算值比对，不存在「第二条起不参与比对」的豁口。
  const declaredPackages = release.artifacts.filter((asset) => asset.kind === 'package');

  // 1. 登记包资产 ↔ 内核复算
  for (const declaredPackage of declaredPackages) {
    const label = declaredPackages.length > 1 ? `资产「${declaredPackage.fileName}」` : '包';
    if (declaredPackage.sha256 !== recomputed.sha256) {
      mismatches.push(
        `${label}哈希不一致：发布单登记 ${declaredPackage.sha256}，本机复算 ${recomputed.sha256}`
      );
    }
    if (declaredPackage.size !== recomputed.size) {
      mismatches.push(`${label}大小不一致：发布单登记 ${declaredPackage.size} 字节，本机复算 ${recomputed.size} 字节`);
    }
  }

  // 2. update-manifest 资产 ↔ 登记资产（双重核对）
  const manifestAssets = release.updateManifest?.assets ?? [];
  let checked = 0;
  for (const manifestAsset of manifestAssets) {
    const fileName = normalizeReleaseText(manifestAsset.fileName ?? '');
    if (!fileName) {
      continue;
    }
    checked += 1;
    // 同名登记资产逐条比对（登记侧已强制 fileName 唯一，此处仍全量遍历兜底）
    const declaredList = release.artifacts.filter((asset) => asset.fileName === fileName);
    if (declaredList.length === 0) {
      mismatches.push(`update-manifest 资产「${fileName}」未在发布单资产清单中登记`);
      continue;
    }
    const manifestSha = normalizeReleaseText(manifestAsset.sha256 ?? '').toLowerCase();
    for (const declared of declaredList) {
      if (manifestSha && manifestSha !== declared.sha256) {
        mismatches.push(`资产「${fileName}」哈希双重核对不一致：update-manifest 声明 ${manifestSha}，发布单登记 ${declared.sha256}`);
      }
      if (typeof manifestAsset.size === 'number' && manifestAsset.size !== declared.size) {
        mismatches.push(`资产「${fileName}」大小双重核对不一致：update-manifest 声明 ${manifestAsset.size} 字节，发布单登记 ${declared.size} 字节`);
      }
      // 包资产：update-manifest 声明值 ↔ 内核复算值
      if (declared.kind === 'package') {
        if (manifestSha && manifestSha !== recomputed.sha256) {
          mismatches.push(`update-manifest 包哈希 ${manifestSha} 与本机复算 ${recomputed.sha256} 不一致`);
        }
        if (typeof manifestAsset.size === 'number' && manifestAsset.size !== recomputed.size) {
          mismatches.push(`update-manifest 包大小 ${manifestAsset.size} 字节与本机复算 ${recomputed.size} 字节不一致`);
        }
      }
    }
  }

  // 3. pluginId / version 三方一致（张冠李戴防护）
  const manifestPluginId = normalizeReleaseText(release.updateManifest?.pluginId ?? '');
  if (manifestPluginId && manifestPluginId !== release.pluginId) {
    mismatches.push(`update-manifest 声明插件「${manifestPluginId}」与发布单目标「${release.pluginId}」不一致`);
  }
  if (recomputed.pluginId && recomputed.pluginId !== release.pluginId) {
    mismatches.push(`导入包内 manifest 插件 id「${recomputed.pluginId}」与发布单目标「${release.pluginId}」不一致`);
  }
  const manifestVersion = normalizeReleaseText(release.updateManifest?.version ?? '');
  if (manifestVersion && manifestVersion !== release.version) {
    mismatches.push(`update-manifest 声明版本「${manifestVersion}」与发布单版本「${release.version}」不一致`);
  }
  if (recomputed.version && recomputed.version !== release.version) {
    mismatches.push(`导入包内 manifest 版本「${recomputed.version}」与发布单版本「${release.version}」不一致`);
  }

  return mismatches;
}

/** 签名材料是否已随发布单声明（sig + pubkey 资产齐备） */
export function hasSignatureMaterial(artifacts: ReleaseArtifact[]): boolean {
  return artifacts.some((asset) => asset.kind === 'sig') && artifacts.some((asset) => asset.kind === 'pubkey');
}

// ------------------------------------------------------------------
// 事件读侧鉴权（伪造状态事件过滤；与 spark-announcement 撤回派生同范式）
// ------------------------------------------------------------------

/**
 * 合法事件操作者集合：发布权配置登记的 publisherRootIds ∪ 名册管理员 rootId。
 * release_events 是 append-only 集合，挡不住自制客户端/库形态直调 sdk.docs.put
 * 写入伪造状态事件——配置不可得（未初始化）时返回空集合 = 全部事件不参与
 * 派生（fail-closed：宁可不派生、不推版本卡片，详情页保留原始时间线兜底）。
 */
export function releaseEventOperatorSet(
  config: ReleaseManagerConfig | null | undefined,
  adminRootIds: Iterable<string> = []
): ReadonlySet<string> {
  const set = new Set<string>();
  if (!config) {
    return set;
  }
  for (const rootId of config.publisherRootIds) {
    set.add(rootId);
  }
  for (const rootId of adminRootIds) {
    set.add(rootId);
  }
  return set;
}

/**
 * 事件读侧过滤：仅合法操作者的事件参与 deriveReleaseState 状态派生与
 * notifyNewReleases 版本卡片补发；非法操作者的事件留痕（append-only 不可删改）
 * 但不参与派生，集合为空 = 全部忽略（fail-closed）。
 */
export function filterAuthorizedReleaseEvents(
  events: ReleaseEvent[],
  operatorRootIds: ReadonlySet<string>
): ReleaseEvent[] {
  return events.filter((event) => operatorRootIds.has(event.operatorRootId));
}

// ------------------------------------------------------------------
// 状态机派生（事件流折叠；确定性排序 at + id 字典序 tie-break）
// ------------------------------------------------------------------

function byDeterministicOrder<T extends { at: number; id: string }>(a: T, b: T): number {
  return a.at - b.at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/**
 * 发布单当前状态（§4 五态，事件流派生）：取该发布单事件流中最新一条状态
 * 事件——撤回一旦追加即为终态标注（append-only 不可删改，已扩散的包不可收回，
 * 渠道视图标注「已撤回」）；无任何事件 = 已登记。
 * 读侧鉴权：调用方须先经 filterAuthorizedReleaseEvents 过滤伪造操作者事件
 * （非法操作者事件不参与派生），本函数不重复校验操作者。
 */
export function deriveReleaseState(releaseId: string, events: ReleaseEvent[]): ReleaseState {
  const ordered = events.filter((event) => event.releaseId === releaseId).sort(byDeterministicOrder);
  const latest = ordered[ordered.length - 1];
  if (!latest) {
    return 'registered';
  }
  switch (latest.type) {
    case 'retracted':
      return 'retracted';
    case 'published':
      return 'published';
    case 'verified':
      return 'verified';
    case 'verify-failed':
      return 'verify-failed';
    case 'channel-pushed': {
      // 渠道推送不改变发布状态：回看最近一条状态事件
      const statusEvent = [...ordered].reverse().find((event) => event.type !== 'channel-pushed');
      if (!statusEvent) {
        return 'registered';
      }
      return statusEvent.type === 'retracted'
        ? 'retracted'
        : statusEvent.type === 'published'
          ? 'published'
          : statusEvent.type === 'verified'
            ? 'verified'
            : 'verify-failed';
    }
  }
}

/** 发布单状态事件时间线（旧→新，确定性排序） */
export function deriveReleaseTimeline(releaseId: string, events: ReleaseEvent[]): ReleaseEvent[] {
  return events.filter((event) => event.releaseId === releaseId).sort(byDeterministicOrder);
}

/** 渠道最近一次推送记录（渠道与更新跟踪区；无推送为 undefined） */
export function latestChannelPush(channelId: string, events: ReleaseEvent[]): ReleaseEvent | undefined {
  const ordered = events
    .filter((event) => event.type === 'channel-pushed' && event.channelId === channelId)
    .sort(byDeterministicOrder);
  return ordered[ordered.length - 1];
}

/**
 * 状态推进的写侧守卫（业务层硬约束：核验未过不得推进「已发布」，失败态也可被看到）：
 * - verify：仅 已登记 / 核验失败（允许复核重验）；
 * - publish：仅 核验通过；
 * - channel-pushed：仅 已发布；
 * - retract：除已撤回外均可（撤回是留痕动作）。
 */
export function canAppendEvent(state: ReleaseState, type: ReleaseEventType): { ok: boolean; reason?: string } {
  switch (type) {
    case 'verified':
    case 'verify-failed':
      if (state !== 'registered' && state !== 'verify-failed') {
        return { ok: false, reason: `当前状态「${RELEASE_STATE_LABELS[state]}」不可再发起核验（核验仅对已登记/核验失败的发布单开放）` };
      }
      return { ok: true };
    case 'published':
      if (state !== 'verified') {
        return { ok: false, reason: '核验未通过不得推进「已发布」（业务层硬约束；失败态也可被看到）' };
      }
      return { ok: true };
    case 'channel-pushed':
      if (state !== 'published') {
        return { ok: false, reason: '仅「已发布」的发布单可登记渠道推送' };
      }
      return { ok: true };
    case 'retracted':
      if (state === 'retracted') {
        return { ok: false, reason: '该发布单已撤回（撤回记录 append-only，无需重复追加）' };
      }
      return { ok: true };
  }
}

// ------------------------------------------------------------------
// 权限（业务层校验；内核不认识「谁能发版」，dev-guide §8 模式）
// ------------------------------------------------------------------

type OrgRole = 'admin' | 'member' | null | undefined;

/**
 * 发布权（§2 角色表）：域身份在发布权配置登记的集合内。配置缺失（尚未初始化）
 * 时 fail-closed——任何成员都无登记路径，由名册管理员先初始化（档三-23）。
 */
export function canPublishRelease(
  config: ReleaseManagerConfig | null | undefined,
  rootId: string | null | undefined
): boolean {
  if (!config || !rootId) {
    return false;
  }
  return config.publisherRootIds.includes(rootId);
}

/** 发布权配置管理（档三-23：MVP 名册管理员直改；演化挂组织治理事务排「规则挂事务」迭代） */
export function canManageReleaseConfig(currentRole: OrgRole): boolean {
  return currentRole === 'admin';
}

/** 撤回权：发布权集合成员或名册管理员（撤回 = 追加记录，不回滚已安装、不删原记录） */
export function canRetractRelease(
  config: ReleaseManagerConfig | null | undefined,
  rootId: string | null | undefined,
  currentRole: OrgRole
): boolean {
  return canPublishRelease(config, rootId) || currentRole === 'admin';
}

// ------------------------------------------------------------------
// 版本分布（档三-25 MVP 骨架；如实标注非全量）
// ------------------------------------------------------------------

/** 版本号比较（semver 三段数值 + 预发布规则简版：无预发布 > 有预发布；非 semver 按字典序兜底） */
export function compareVersions(a: string, b: string): number {
  const parse = (version: string): { nums: number[]; pre: string } | null => {
    const match = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?/.exec(version);
    if (!match) {
      return null;
    }
    return { nums: [Number(match[1]), Number(match[2]), Number(match[3])], pre: match[4] ?? '' };
  };
  const pa = parse(a);
  const pb = parse(b);
  if (!pa || !pb) {
    return a < b ? -1 : a > b ? 1 : 0;
  }
  for (let i = 0; i < 3; i += 1) {
    if (pa.nums[i] !== pb.nums[i]) {
      return pa.nums[i] - pb.nums[i];
    }
  }
  if (pa.pre === pb.pre) {
    return 0;
  }
  if (!pa.pre) {
    return 1;
  }
  if (!pb.pre) {
    return -1;
  }
  return pa.pre < pb.pre ? -1 : 1;
}

/** 单个目标插件的版本分布视图（计数分布 + 信任级分布 + 滞后率；骨架口径） */
export type VersionDistribution = {
  pluginId: string;
  /** 最新已上报版本（compareVersions 最大者） */
  latestVersion: string | null;
  /** 各版本上报人数（按版本降序） */
  versions: Array<{ version: string; count: number }>;
  /** 信任级分布（原样统计 signed / repo-anchored / sideloaded / 其他） */
  trusts: Array<{ trust: string; count: number }>;
  /** 上报总人数（每成员取最新一条上报） */
  reporterCount: number;
  /** 滞后人数（最新上报版本落后于 latestVersion 的成员数） */
  behindCount: number;
};

/**
 * 版本分布派生：同一成员多条上报取最新一条（at + id 字典序 tie-break，跨设备
 * 确定性）；只显示计数分布——单条记录的查询权限口径待评审（§9），视图层据此
 * 如实标注「仅含 opt-in 上报成员，非全量」。
 */
export function deriveVersionDistribution(pluginId: string, reports: VersionReport[]): VersionDistribution {
  const latestByReporter = new Map<string, VersionReport>();
  for (const report of reports.filter((item) => item.pluginId === pluginId).sort(byDeterministicOrder)) {
    latestByReporter.set(report.reporterRootId, report);
  }
  const effective = [...latestByReporter.values()];
  const byVersion = new Map<string, number>();
  const byTrust = new Map<string, number>();
  for (const report of effective) {
    byVersion.set(report.version, (byVersion.get(report.version) ?? 0) + 1);
    byTrust.set(report.trust, (byTrust.get(report.trust) ?? 0) + 1);
  }
  const versions = [...byVersion.entries()]
    .map(([version, count]) => ({ version, count }))
    .sort((a, b) => compareVersions(b.version, a.version) || (a.version < b.version ? -1 : 1));
  const latestVersion = versions[0]?.version ?? null;
  const behindCount = latestVersion
    ? effective.filter((report) => compareVersions(report.version, latestVersion) < 0).length
    : 0;
  return {
    pluginId,
    latestVersion,
    versions,
    trusts: [...byTrust.entries()]
      .map(([trust, count]) => ({ trust, count }))
      .sort((a, b) => b.count - a.count || (a.trust < b.trust ? -1 : 1)),
    reporterCount: effective.length,
    behindCount
  };
}

// ------------------------------------------------------------------
// 签名载荷与通知摘要
// ------------------------------------------------------------------

/**
 * 内容哈希（FNV-1a 32bit，hex；与 spark-forum/spark-kanban 同一实现有意重复——
 * 插件唯一依赖 plugin-sdk）：只承担签名载荷压缩，防抵赖强度由 Ed25519 域签名
 * 保证；插件沙箱不假设 WebCrypto 可用（opaque origin iframe），故纯 TS 实现。
 */
export function hashReleaseContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/**
 * 签名载荷四元绑定（spark-forum buildForumSignPayload 同模式）：
 * `{orgId}:{recordId}:{operatorRootId}:{内容哈希}`——签名绑定「谁在哪个组织以
 * 哪个身份写了哪条记录」，验签侧从记录当前字段重算比对，无法剪贴重放。
 */
export function buildReleaseSignPayload(
  orgId: string,
  recordId: string,
  operatorRootId: string,
  content: string
): string {
  return `${orgId}:${recordId}:${operatorRootId}:${hashReleaseContent(content)}`;
}

/**
 * 发布单签名内容：目标插件 + 版本 + 全部资产（kind/fileName/sha256/size 逐条）
 * + 变更说明引用 + 关联事务 + 渠道全量编入——任一字段被替换即验签失配。
 * 包哈希由此随签名入存证链（governance 集合链式存证 + 域签名防抵赖双保险）。
 */
export function releaseSignContent(input: {
  pluginId: string;
  version: string;
  artifacts: ReleaseArtifact[];
  changelog?: string;
  changelogRef?: string;
  decisionRef?: string;
  channels: string[];
}): string {
  const artifactLines = input.artifacts
    .map((asset) => `${asset.kind}|${asset.fileName}|${asset.sha256}|${asset.size}|${asset.url ?? ''}`)
    .join('\n');
  return [
    input.pluginId,
    input.version,
    artifactLines,
    input.changelog ?? '',
    input.changelogRef ?? '',
    input.decisionRef ?? '',
    [...input.channels].sort().join(',')
  ].join('\n---\n');
}

/** 状态事件签名内容：绑定「谁对哪条发布单推进了哪类状态、原因/引用是什么」 */
export function releaseEventSignContent(event: {
  releaseId: string;
  type: ReleaseEventType;
  channelId?: string;
  resultRef?: string;
  reason?: string;
  detail?: string;
}): string {
  return [
    event.releaseId,
    event.type,
    event.channelId ?? '',
    event.resultRef ?? '',
    event.reason ?? '',
    event.detail ?? ''
  ].join('\n');
}

// ------------------------------------------------------------------
// summary 纪律（≤200 字符、自含完整语义；未装插件时壳层原生渲染的保底文本）
// ------------------------------------------------------------------

/** 码点安全截断到 max（超限补省略号；surrogate 对不被劈开） */
function truncateSummary(text: string, max: number): string {
  const chars = [...text];
  return chars.length <= max ? text : `${chars.slice(0, max - 1).join('')}…`;
}

/** 目标插件显示名（仓库地址形态取末段，卡片摘要可读性） */
export function pluginDisplayName(pluginId: string): string {
  const trimmed = normalizeReleaseText(pluginId);
  const tail = trimmed.split('/').filter(Boolean).pop();
  return (tail ?? trimmed).replace(/\.git$/, '') || trimmed;
}

/**
 * 版本发布卡片摘要（档一-2：本插件唯一推送；releaseRef/版本号幂等键）：
 * `【版本发布·{插件} v{version}】{变更说明首部…}`，trim 后 ≤200 字符自成完整语义。
 */
export function buildReleaseCardSummary(input: { pluginId: string; version: string; changelog?: string }): string {
  const prefix = `【版本发布·${pluginDisplayName(input.pluginId)} v${input.version}】`;
  const changelogPreview = normalizeReleaseText(input.changelog ?? '').replace(/\s+/g, ' ');
  let summary = prefix;
  const remaining = RELEASE_SUMMARY_LIMIT - [...summary].length;
  if (changelogPreview && remaining > 2) {
    const budget = remaining - 1;
    const excerpt = [...changelogPreview].slice(0, budget).join('');
    summary += `${excerpt}${[...changelogPreview].length > budget ? '…' : ''}`;
  }
  if ([...summary].length > RELEASE_SUMMARY_LIMIT) {
    summary = truncateSummary(summary, RELEASE_SUMMARY_LIMIT);
  }
  return summary;
}

/** 撤回告知摘要（档三-26：撤回 = 组织内登记 + 公告告知，不回滚已安装） */
export function buildRetractionSummary(input: { pluginId: string; version: string; reason?: string }): string {
  const prefix = `【版本撤回·${pluginDisplayName(input.pluginId)} v${input.version}】`;
  const reasonPreview = normalizeReleaseText(input.reason ?? '').replace(/\s+/g, ' ');
  let summary = reasonPreview ? `${prefix}${reasonPreview}` : `${prefix}该版本已被发布方撤回，已安装的用户不会被回滚，请暂缓更新并关注后续公告。`;
  if ([...summary].length > RELEASE_SUMMARY_LIMIT) {
    summary = truncateSummary(summary, RELEASE_SUMMARY_LIMIT);
  }
  return summary;
}

/** 「另有 N 个历史版本发布」汇总消息（补发节流防刷屏，同公告插件口径） */
export function buildReleaseHistorySummary(count: number): string {
  return `【发布】另有 ${count} 个历史版本发布未逐条推送，请打开「发布」插件查看完整列表。`;
}

// ------------------------------------------------------------------
// 补发节流（档二-4 MVP 降级：插件加载时补发 + 节流；同公告插件限流预算）
// ------------------------------------------------------------------

export type ReleaseBackfillBatch = {
  /** 本轮要逐条生成卡片的发布单（升序） */
  cards: ReleaseRecord[];
  /** 被汇总覆盖的发布单数（>0 时需追加一条汇总消息） */
  summarizedCount: number;
  /** 被汇总覆盖、直接记账的发布单 */
  summarized: ReleaseRecord[];
};

export function selectReleaseBackfillBatch(pending: ReleaseRecord[]): ReleaseBackfillBatch {
  if (pending.length <= RELEASE_BACKFILL_FULL_THRESHOLD) {
    return { cards: pending, summarizedCount: 0, summarized: [] };
  }
  const summarized = pending.slice(0, pending.length - 1);
  return { cards: pending.slice(pending.length - 1), summarizedCount: summarized.length, summarized };
}
