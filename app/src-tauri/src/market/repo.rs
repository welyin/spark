//! 仓库锚定安装（protocol wiki/protocol/plugin-dist.md，阶段 C 波次 1）。
//!
//! - 插件 id = 规范化开源仓库地址（`{host}/{owner}/{repo}[/path]`，host 限
//!   github.com/gitlab.com/gitee.com），仓库声明文件 spark-plugin.json 为信任锚点；
//! - 验证三步：解析规范化 id → 从该仓库取声明文件（双源交叉）→ id 一致性校验；
//!   包内 manifest 自证不作数（规格 §4.1）；
//! - 签名可选：有 .sig 必验（沿用 trust.rs 信任链），无 .sig 走双源交叉 +
//!   逐文件 sha256，安装状态标记 trust = "repo-anchored"；
//! - 网络层抽象为 [`RepoFetcher`] trait（单测 mock；生产为 reqwest blocking），
//!   镜像不可信仅作通路：origin → 声明文件 mirrors[] → 内置公共镜像（gh-proxy 系）。
//!
//! 声明文件缓存：内存 TTL 10 分钟 + sled 持久化（键 `plugin:repo:<规范化 id>`，
//! 规格 §4.4）；sled 按次开库即用即关，避免与内核/其他实例的目录独占冲突。

use std::path::Path;

use serde::{Deserialize, Serialize};
use spark_core::storage::{SledStorage, StorageBackend};

use super::permissions::{normalize_declared_permissions, resolve_granted_permissions};
use super::sources::now_millis;
use super::types::{InstalledPluginState, PluginReleaseManifest, PluginUpdateProbe};
use super::{PluginMarketService, trust};

// ------------------------------------------------------------------
// 常量（规格 §1/§2/§3.3/§4.4）
// ------------------------------------------------------------------

/// 允许的托管平台（规格 §1.1）。
const ALLOWED_HOSTS: [&str; 3] = ["github.com", "gitlab.com", "gitee.com"];
/// 声明文件固定文件名（规格 §2）。
const DECLARATION_FILE_NAME: &str = "spark-plugin.json";
/// 声明文件大小上限 64 KiB（规格 §2.1）。
const DECLARATION_MAX_BYTES: usize = 64 * 1024;
/// 内置公共镜像前缀（gh-proxy 系，仅 github.com origin 适用，规格 §3.3）。
const PUBLIC_MIRROR_PREFIXES: [&str; 2] = ["https://mirror.ghproxy.com/", "https://gh-proxy.com/"];
/// 声明文件缓存 sled 键前缀（规格 §4.4）。
const REPO_CACHE_KEY_PREFIX: &str = "plugin:repo:";
/// 声明文件内存缓存 TTL：10 分钟（规格 §4.4）。
const DECLARATION_CACHE_TTL_MS: u64 = 10 * 60 * 1000;

// ------------------------------------------------------------------
// 插件 id（规格 §1）
// ------------------------------------------------------------------

/// 规范化仓库 id（构造即合法；`normalized()` 输出即协议线形）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoId {
    pub host: String,
    pub owner: String,
    pub repo: String,
    /// monorepo 子路径段（单插件仓库为空）
    pub path: Vec<String>,
}

fn id_invalid(input: &str) -> String {
    format!("Repo plugin id invalid: {input}")
}

/// 段字符集 `[a-z0-9._-]`（规格 §1.1；输入已小写化）。
fn segment_valid(segment: &str, max_len: usize) -> bool {
    !segment.is_empty()
        && segment.len() <= max_len
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

impl RepoId {
    /// 解析 + 规范化（规格 §1.2）：去空白/ scheme 前缀/尾斜杠、repo 去 `.git`、
    /// 全串小写，随后按 §1.1 校验。输入容忍 `https?://` 前缀（不参与抓取）。
    pub fn parse(input: &str) -> Result<RepoId, String> {
        let mut text = input.trim().to_string();
        // scheme 前缀容忍一次（大小写不敏感；仅作输入清洗，不参与任何抓取）
        for prefix in ["https://", "http://"] {
            if text.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix)) {
                text = text[prefix.len()..].to_string();
                break;
            }
        }
        let text = text.trim_end_matches('/').to_lowercase();
        if text.is_empty() || text.len() > 256 {
            return Err(id_invalid(input));
        }
        let mut segments: Vec<&str> = text.split('/').collect();
        // repo 段去一次 `.git` 后缀
        if segments.len() >= 3 {
            if let Some(repo) = segments[2].strip_suffix(".git") {
                segments[2] = repo;
            }
        }
        if segments.len() < 3 || segments.len() > 3 + 8 {
            return Err(id_invalid(input));
        }
        let host = segments[0].to_string();
        if !ALLOWED_HOSTS.contains(&host.as_str()) {
            return Err(id_invalid(input));
        }
        if !segment_valid(segments[1], 100) || !segment_valid(segments[2], 100) {
            return Err(id_invalid(input));
        }
        let path: Vec<String> = segments[3..].iter().map(|s| s.to_string()).collect();
        if !path.iter().all(|s| segment_valid(s, 64)) {
            return Err(id_invalid(input));
        }
        Ok(RepoId {
            host,
            owner: segments[1].to_string(),
            repo: segments[2].to_string(),
            path,
        })
    }

    /// 规范化线形：`{host}/{owner}/{repo}[/path]`。
    pub fn normalized(&self) -> String {
        let mut out = format!("{}/{}/{}", self.host, self.owner, self.repo);
        for segment in &self.path {
            out.push('/');
            out.push_str(segment);
        }
        out
    }

    /// 声明文件目录前缀（sub-path 段加 `/`，根仓库为空串）。
    fn dir_prefix(&self) -> String {
        if self.path.is_empty() {
            String::new()
        } else {
            format!("{}/", self.path.join("/"))
        }
    }

    /// release tag（规格 §2.2）：根仓库 `v<version>`，monorepo `<末段>-v<version>`。
    fn release_tag(&self, version: &str) -> String {
        match self.path.last() {
            Some(last) => format!("{last}-v{version}"),
            None => format!("v{version}"),
        }
    }

    /// release 资产形式的声明文件名（规格 §3.1）。
    fn declaration_asset_name(&self) -> String {
        match self.path.last() {
            Some(last) => format!("{last}-{DECLARATION_FILE_NAME}"),
            None => DECLARATION_FILE_NAME.to_string(),
        }
    }

    /// 镜像条目只取 host/owner/repo 三段，sub-path 沿用原 id（规格 §3.2）。
    fn with_mirror_base(&self, mirror: &RepoId) -> RepoId {
        RepoId {
            host: mirror.host.clone(),
            owner: mirror.owner.clone(),
            repo: mirror.repo.clone(),
            path: self.path.clone(),
        }
    }
}

// ------------------------------------------------------------------
// 仓库声明文件 spark-plugin.json（规格 §2）
// ------------------------------------------------------------------

/// 仓库声明文件（规格 §2.1；未知字段忽略，前向兼容）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SparkPluginDeclaration {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub summary: String,
    pub category: String,
    pub version: String,
    pub release_asset_pattern: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// 插件支持的空间类型（"personal" / "org" 子集，非空；可选，
    /// 缺省按 ["org"] 处理——spaces-and-plugins §4，规格 §2.1）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_spaces: Option<Vec<String>>,
    pub sdk_version: String,
}

/// semver 三段形状（x.y.z，可带 `-预发布` / `+build` 元数据；§2.1）。
fn version_shape_valid(version: &str) -> bool {
    if version.is_empty() || version.chars().count() > 32 {
        return false;
    }
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// 声明文件字段与大小校验 + id 一致性（规格 §2.1/§4.1）。
fn validate_declaration(raw_text: &str, expected: &RepoId) -> Result<SparkPluginDeclaration, String> {
    let invalid = |reason: &str| {
        format!(
            "Repo plugin declaration invalid: {}: {reason}",
            expected.normalized()
        )
    };
    if raw_text.len() > DECLARATION_MAX_BYTES {
        return Err(invalid("declaration exceeds 64 KiB"));
    }
    let mut declaration: SparkPluginDeclaration =
        serde_json::from_str(raw_text).map_err(|e| invalid(&format!("{e}")))?;
    if declaration.name.is_empty() || declaration.name.chars().count() > 64 {
        return Err(invalid("name length out of range"));
    }
    if declaration.summary.is_empty() || declaration.summary.chars().count() > 256 {
        return Err(invalid("summary length out of range"));
    }
    // icon：data: base64 ≤ 20 KB 或 https URL 或空（base64 膨胀系数 4/3，放宽到 28 KB 字符）
    if declaration.icon.len() > 28 * 1024 {
        return Err(invalid("icon exceeds 20 KB"));
    }
    if !declaration.icon.is_empty()
        && !declaration.icon.starts_with("data:")
        && !declaration.icon.starts_with("https://")
    {
        return Err(invalid("icon must be data: base64 or https URL"));
    }
    // version：semver 三段形状（§2.1）
    if !version_shape_valid(&declaration.version) {
        return Err(invalid("version not semver x.y.z"));
    }
    if declaration.mirrors.len() > 8 || declaration.mirrors.iter().any(|m| RepoId::parse(m).is_err()) {
        return Err(invalid("mirrors invalid"));
    }
    // releaseAssetPattern：必须含一次 <version> 且以 .spkg 结尾（规格 §2.1）
    if declaration.release_asset_pattern.matches("<version>").count() != 1
        || !declaration.release_asset_pattern.ends_with(".spkg")
    {
        return Err(invalid("releaseAssetPattern invalid"));
    }
    // supportedSpaces（规格 §2.1）：可选；声明时必须非空且只含 personal/org，重复值去重
    if let Some(spaces) = &mut declaration.supported_spaces {
        if spaces.is_empty()
            || spaces
                .iter()
                .any(|space| space != "personal" && space != "org")
        {
            return Err(invalid("supportedSpaces invalid"));
        }
        // 值域仅 personal/org 两个，线性查重即可（保持首次出现顺序）
        let mut seen: Vec<String> = Vec::with_capacity(spaces.len());
        spaces.retain(|space| {
            let first_seen = !seen.contains(space);
            seen.push(space.clone());
            first_seen
        });
    }
    // id 一致性（规格 §4.1-4）：声明文件 id 规范化后必须等于所在仓库地址
    let declared_id = RepoId::parse(&declaration.id).map_err(|_| {
        format!(
            "Repo plugin declaration id mismatch: expected {}, got {}",
            expected.normalized(),
            declaration.id
        )
    })?;
    if declared_id.normalized() != expected.normalized() {
        return Err(format!(
            "Repo plugin declaration id mismatch: expected {}, got {}",
            expected.normalized(),
            declaration.id
        ));
    }
    Ok(declaration)
}

/// 由 releaseAssetPattern 派生（tag, 包资产, 清单资产, 签名资产）（规格 §2.2）。
fn derive_release_names(declaration: &SparkPluginDeclaration, id: &RepoId) -> (String, String, String, String) {
    let pattern = &declaration.release_asset_pattern;
    let package = pattern.replace("<version>", &declaration.version);
    // 去结尾 .spkg 换扩展名（不做全串 replace：包名中段可能出现 ".spkg" 字样）
    let manifest_stem = pattern.replace("<version>", "manifest");
    let stem = manifest_stem.strip_suffix(".spkg").unwrap_or(&manifest_stem);
    (
        id.release_tag(&declaration.version),
        package,
        format!("{stem}.json"),
        format!("{stem}.sig"),
    )
}

// ------------------------------------------------------------------
// URL 模板与镜像展开（规格 §3）
// ------------------------------------------------------------------

/// 源族（规格 §3.4 交叉比对的"独立源"口径：origin / 声明镜像 / 内置公共镜像互为异族）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceFamily {
    Origin,
    Declared,
    Public,
}

/// 声明文件 origin URL 族（规格 §3.1）：release 资产（latest 指针）优先，raw 兜底。
fn declaration_origin_urls(id: &RepoId) -> Vec<String> {
    let dir = id.dir_prefix();
    let asset = id.declaration_asset_name();
    let (owner, repo) = (&id.owner, &id.repo);
    match id.host.as_str() {
        "github.com" => vec![
            format!("https://github.com/{owner}/{repo}/releases/latest/download/{asset}"),
            format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{dir}{DECLARATION_FILE_NAME}"),
        ],
        "gitlab.com" => vec![
            format!("https://gitlab.com/{owner}/{repo}/-/releases/permalink/latest/downloads/{asset}"),
            format!("https://gitlab.com/{owner}/{repo}/-/raw/main/{dir}{DECLARATION_FILE_NAME}"),
            format!("https://gitlab.com/{owner}/{repo}/-/raw/master/{dir}{DECLARATION_FILE_NAME}"),
        ],
        // gitee.com
        _ => vec![
            format!("https://gitee.com/{owner}/{repo}/releases/latest/download/{asset}"),
            format!("https://gitee.com/{owner}/{repo}/raw/main/{dir}{DECLARATION_FILE_NAME}"),
            format!("https://gitee.com/{owner}/{repo}/raw/master/{dir}{DECLARATION_FILE_NAME}"),
        ],
    }
}

/// release 资产 origin URL（规格 §3.1）。
fn release_asset_origin_url(id: &RepoId, tag: &str, asset: &str) -> String {
    let (owner, repo) = (&id.owner, &id.repo);
    match id.host.as_str() {
        "github.com" => format!("https://github.com/{owner}/{repo}/releases/download/{tag}/{asset}"),
        "gitlab.com" => format!("https://gitlab.com/{owner}/{repo}/-/releases/{tag}/downloads/{asset}"),
        _ => format!("https://gitee.com/{owner}/{repo}/releases/download/{tag}/{asset}"),
    }
}

/// 内置公共镜像展开（规格 §3.3）：仅 github 系 URL，逐前缀包裹，保持原顺序。
fn expand_public_mirrors(urls: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for url in urls {
        if url.starts_with("https://github.com/") || url.starts_with("https://raw.githubusercontent.com/") {
            for prefix in PUBLIC_MIRROR_PREFIXES {
                out.push(format!("{prefix}{url}"));
            }
        }
    }
    out
}

/// 声明文件源列表（规格 §3.2 首轮：origin + 内置公共镜像两族）。
fn declaration_sources(id: &RepoId) -> Vec<(SourceFamily, String)> {
    let origin = declaration_origin_urls(id);
    let mut sources: Vec<(SourceFamily, String)> =
        origin.iter().cloned().map(|u| (SourceFamily::Origin, u)).collect();
    sources.extend(
        expand_public_mirrors(&origin)
            .into_iter()
            .map(|u| (SourceFamily::Public, u)),
    );
    sources
}

/// release 资产源列表：origin → 声明文件 mirrors[] → 内置公共镜像（规格 §3.2）。
fn release_asset_sources(
    id: &RepoId,
    mirrors: &[String],
    tag: &str,
    asset: &str,
) -> Vec<(SourceFamily, String)> {
    let origin_url = release_asset_origin_url(id, tag, asset);
    let mut sources = vec![(SourceFamily::Origin, origin_url.clone())];
    for mirror in mirrors {
        // mirrors 已经过声明文件校验，parse 必成功
        if let Ok(mirror_id) = RepoId::parse(mirror) {
            sources.push((
                SourceFamily::Declared,
                release_asset_origin_url(&id.with_mirror_base(&mirror_id), tag, asset),
            ));
        }
    }
    sources.extend(
        expand_public_mirrors(std::slice::from_ref(&origin_url))
            .into_iter()
            .map(|u| (SourceFamily::Public, u)),
    );
    sources
}

// ------------------------------------------------------------------
// 抓取抽象（单测 mock；生产 reqwest blocking）
// ------------------------------------------------------------------

/// 更新清单文本上限（1 MiB；清单只含资产条目，远小于此，防无界响应）。
const MANIFEST_MAX_BYTES: u64 = 1024 * 1024;
/// 签名文本上限（16 KiB；base64 签名百余字节，留余量）。
const SIGNATURE_MAX_BYTES: u64 = 16 * 1024;

/// 文本抓取器：`Ok(None)` = 404（资产不存在），`Err` = 网络/协议错误；
/// 响应体超 `max_bytes` 即拒（流式截断，声明文件 64 KiB+1 即拒由此落实）。
pub trait RepoFetcher {
    fn fetch_text(&self, url: &str, max_bytes: u64) -> Result<Option<String>, String>;
    /// 包体字节抓取：Content-Length 超 max_bytes（= 清单登记 size）即断。
    fn fetch_bytes(&self, url: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, String>;
}

/// 生产抓取器：复用 sources 层共享 reqwest blocking client（系统信任库 + 超时）。
pub struct HttpRepoFetcher;

impl RepoFetcher for HttpRepoFetcher {
    fn fetch_text(&self, url: &str, max_bytes: u64) -> Result<Option<String>, String> {
        super::sources::fetch_text_http_optional(url, max_bytes)
    }

    fn fetch_bytes(&self, url: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
        super::sources::fetch_bytes_http_optional(url, max_bytes)
    }
}

/// 顺序取首个成功（Ok(Some)）；全部 404 → Ok(None)；全部错误 → Err（最后一个错误）。
fn fetch_first(
    fetcher: &dyn RepoFetcher,
    sources: &[(SourceFamily, String)],
    max_bytes: u64,
) -> Result<Option<String>, String> {
    let mut saw_not_found = false;
    let mut last_error = None;
    for (_, url) in sources {
        match fetcher.fetch_text(url, max_bytes) {
            Ok(Some(text)) => return Ok(Some(text)),
            Ok(None) => saw_not_found = true,
            Err(e) => last_error = Some(e),
        }
    }
    if saw_not_found {
        Ok(None)
    } else {
        Err(last_error.unwrap_or_else(|| "no source reachable".to_string()))
    }
}

/// 双源交叉抓取（规格 §3.4）：首份成功后继续取**异族**第二份，字节不一致即拒；
/// 仅一族源可达时按 `require_origin_single` 裁决（声明文件/无签名清单仅 origin 可单源）。
fn fetch_with_cross_check(
    fetcher: &dyn RepoFetcher,
    sources: &[(SourceFamily, String)],
    require_origin_single: bool,
    max_bytes: u64,
    cross_mismatch_error: &str,
    fetch_failed_error: &str,
) -> Result<String, String> {
    let mut first: Option<(SourceFamily, String)> = None;
    for (family, url) in sources {
        let Ok(Some(text)) = fetcher.fetch_text(url, max_bytes) else {
            continue;
        };
        match &first {
            None => first = Some((*family, text)),
            Some((first_family, first_text)) => {
                if *first_family != *family {
                    if *first_text != text {
                        return Err(cross_mismatch_error.to_string());
                    }
                    return Ok(first_text.clone());
                }
            }
        }
    }
    match first {
        Some((family, text)) if !require_origin_single || family == SourceFamily::Origin => Ok(text),
        _ => Err(fetch_failed_error.to_string()),
    }
}

// ------------------------------------------------------------------
// 声明文件缓存（规格 §4.4：内存 TTL 10 分钟 + sled 持久化）
// ------------------------------------------------------------------

/// 缓存条目（sled 值 JSON 线形：`{"fetchedAt":<ms>,"text":<原文>}`）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CachedRepoDeclaration {
    pub fetched_at: u64,
    pub text: String,
}

fn cache_key(normalized_id: &str) -> String {
    format!("{REPO_CACHE_KEY_PREFIX}{normalized_id}")
}

/// sled 按次开库（即用即关，写后 flush；失败一律降级为无持久化缓存）。
fn sled_cache_get(cache_dir: &Path, normalized_id: &str) -> Option<CachedRepoDeclaration> {
    let db = SledStorage::open(cache_dir).ok()?;
    let raw = db.get(&cache_key(normalized_id)).ok()??;
    serde_json::from_str(&raw).ok()
}

fn sled_cache_put(cache_dir: &Path, normalized_id: &str, entry: &CachedRepoDeclaration) {
    if let Ok(mut db) = SledStorage::open(cache_dir) {
        if let Ok(text) = serde_json::to_string(entry) {
            let _ = db.put(&cache_key(normalized_id), &text);
            let _ = db.flush();
        }
    }
}

// ------------------------------------------------------------------
// 服务方法：声明解析 / 仓库锚定安装
// ------------------------------------------------------------------

impl PluginMarketService {
    /// 预览用：解析仓库声明文件（含缓存；前端「按仓库地址安装」确认前展示）。
    /// 返回声明文件副本，id 字段替换为规范化线形。
    pub fn resolve_repo_plugin(&mut self, id: &str) -> Result<SparkPluginDeclaration, String> {
        self.resolve_repo_plugin_with(&HttpRepoFetcher, id)
    }

    pub(crate) fn resolve_repo_plugin_with(
        &mut self,
        fetcher: &dyn RepoFetcher,
        id: &str,
    ) -> Result<SparkPluginDeclaration, String> {
        let repo_id = RepoId::parse(id)?;
        let mut declaration = self.fetch_repo_declaration(fetcher, &repo_id)?;
        declaration.id = repo_id.normalized();
        Ok(declaration)
    }

    /// 取声明文件：内存（TTL 10 分钟）→ sled → 网络双源交叉；新副本双写缓存。
    fn fetch_repo_declaration(
        &mut self,
        fetcher: &dyn RepoFetcher,
        repo_id: &RepoId,
    ) -> Result<SparkPluginDeclaration, String> {
        let key = repo_id.normalized();
        if let Some(cached) = self.repo_decl_cache.get(&key) {
            if now_millis().saturating_sub(cached.fetched_at) < DECLARATION_CACHE_TTL_MS {
                return validate_declaration(&cached.text, repo_id);
            }
        }
        if let Some(cached) = sled_cache_get(&self.paths.repo_cache_dir, &key) {
            self.repo_decl_cache.insert(key.clone(), cached.clone());
            return validate_declaration(&cached.text, repo_id);
        }
        let text = fetch_with_cross_check(
            fetcher,
            &declaration_sources(repo_id),
            true,
            DECLARATION_MAX_BYTES as u64,
            &format!("Repo plugin declaration cross-check mismatch: {key}"),
            &format!("Repo plugin declaration fetch failed: {key}"),
        )?;
        let declaration = validate_declaration(&text, repo_id)?;
        let entry = CachedRepoDeclaration {
            fetched_at: now_millis(),
            text,
        };
        self.repo_decl_cache.insert(key.clone(), entry.clone());
        sled_cache_put(&self.paths.repo_cache_dir, &key, &entry);
        Ok(declaration)
    }

    /// 市场列表合成用：只读缓存中的声明文件（不触网）。
    pub(crate) fn peek_cached_declaration(&self, normalized_id: &str) -> Option<SparkPluginDeclaration> {
        let repo_id = RepoId::parse(normalized_id).ok()?;
        let cached = self
            .repo_decl_cache
            .get(normalized_id)
            .cloned()
            .or_else(|| sled_cache_get(&self.paths.repo_cache_dir, normalized_id))?;
        validate_declaration(&cached.text, &repo_id).ok()
    }

    /// 仓库锚定安装（规格 §4.2）：声明 → 派生资产名 → 清单（有 sig 验签 /
    /// 无 sig 双源交叉）→ 复用现有下载 + 逐文件 sha256 链路 → 落状态。
    pub fn install_from_repo(&mut self, id: &str) -> Result<InstalledPluginState, String> {
        self.install_from_repo_with(&HttpRepoFetcher, id)
    }

    pub(crate) fn install_from_repo_with(
        &mut self,
        fetcher: &dyn RepoFetcher,
        id: &str,
    ) -> Result<InstalledPluginState, String> {
        let repo_id = RepoId::parse(id)?;
        let plugin_id = repo_id.normalized();
        let declaration = self.fetch_repo_declaration(fetcher, &repo_id)?;
        let (tag, _package_name, manifest_asset, signature_asset) =
            derive_release_names(&declaration, &repo_id);

        // 签名可选层（规格 §4.3）：取到 .sig 必验；取不到（全 404/不可达）走交叉
        let manifest_sources = release_asset_sources(&repo_id, &declaration.mirrors, &tag, &manifest_asset);
        let signature_sources =
            release_asset_sources(&repo_id, &declaration.mirrors, &tag, &signature_asset);
        // 网络错误与 404 同等按"无签名资产"处理：签名为可选增强层，
        // 降级后仍须过 origin 双源交叉 + 包体 sha256，不降低锚定强度
        let signature = fetch_first(fetcher, &signature_sources, SIGNATURE_MAX_BYTES)
            .ok()
            .flatten();

        let (manifest_text, trust_level) = match signature {
            Some(signature_text) => {
                let text = fetch_first(fetcher, &manifest_sources, MANIFEST_MAX_BYTES)
                    .ok()
                    .flatten()
                    .ok_or_else(|| format!("Repo plugin manifest fetch failed: {plugin_id}"))?;
                if !trust::verify_manifest_signature(&text, signature_text.trim(), &self.trust_keys) {
                    return Err(format!(
                        "Repo plugin manifest signature verification failed: {plugin_id}"
                    ));
                }
                (text, "signed")
            }
            None => {
                let text = fetch_with_cross_check(
                    fetcher,
                    &manifest_sources,
                    true,
                    MANIFEST_MAX_BYTES,
                    &format!("Repo plugin manifest cross-check mismatch: {plugin_id}"),
                    &format!("Repo plugin manifest fetch failed: {plugin_id}"),
                )?;
                (text, "repo-anchored")
            }
        };

        let manifest: PluginReleaseManifest = serde_json::from_str(&manifest_text).map_err(|e| {
            format!("Repo plugin manifest invalid: {plugin_id}: {e}")
        })?;
        if manifest.plugin_id != plugin_id {
            return Err(format!(
                "Repo plugin manifest id mismatch: expected {plugin_id}, got {}",
                manifest.plugin_id
            ));
        }
        if manifest.version != declaration.version {
            return Err(format!(
                "Repo plugin manifest version mismatch: expected {}, got {}",
                declaration.version, manifest.version
            ));
        }
        let asset = manifest
            .package_asset()
            .ok_or_else(|| format!("Repo plugin manifest invalid: {plugin_id}: no package asset"))?
            .clone();

        // 远程清单资产 URL 仅 https（§3.2-3：file:// 只属内置目录本地 bundle 链路，
        // 远程清单指向本地文件一律拒——防借清单读本地文件落盘）
        if !asset.url.starts_with("https://") {
            return Err(format!(
                "Repo plugin manifest invalid: {plugin_id}: package asset url must be https"
            ));
        }
        // 包体经抓取层有界下载（Content-Length 超 size 即断）→ 校验 → 落盘（规格 §5）
        let bytes = fetcher
            .fetch_bytes(&asset.url, asset.size)?
            .ok_or_else(|| format!("Repo plugin package fetch failed: {plugin_id}"))?;
        let (file_path, digest, size) = self.save_verified_package_bytes(&asset, &plugin_id, &bytes)?;
        // grantedPermissions = 基础 ∪ 声明∩高级：清单声明优先，缺省用声明文件（规格 §5）
        let declared = match manifest.permissions.as_ref() {
            Some(raw) => normalize_declared_permissions(raw),
            None => normalize_declared_permissions(&declaration.permissions),
        };
        let installed_state = InstalledPluginState {
            plugin_id: plugin_id.clone(),
            version: manifest.version.clone(),
            package_path: file_path.to_string_lossy().to_string(),
            sha256: digest,
            size,
            installed_at: now_millis(),
            enabled: true,
            granted_permissions: resolve_granted_permissions(&declared),
            trust: Some(trust_level.to_string()),
            supported_spaces: declaration.supported_spaces.clone(),
        };
        self.state
            .installed
            .insert(plugin_id.clone(), installed_state.clone());
        // 显式安装成功 → 清除卸载墓碑
        self.state.uninstalled.remove(&plugin_id);
        self.update_probes.insert(
            plugin_id.clone(),
            PluginUpdateProbe {
                plugin_id: plugin_id.clone(),
                checked_at: now_millis(),
                latest_version: Some(manifest.version),
                update_available: false,
                reason: "installed".to_string(),
            },
        );
        if let Err(e) = self.persist() {
            // 持久化失败回滚内存插入，避免内存态与磁盘状态文件不一致
            self.state.installed.remove(&plugin_id);
            self.update_probes.remove(&plugin_id);
            return Err(e);
        }
        Ok(installed_state)
    }
}

/// 已安装但不在内置目录中的条目（仓库锚定安装）合成市场目录条目（updates.rs 用）。
pub(crate) fn synthesize_catalog_entry(
    service: &PluginMarketService,
    installed: &InstalledPluginState,
) -> super::catalog::PluginCatalogItem {
    let declaration = service.peek_cached_declaration(&installed.plugin_id);
    super::catalog::PluginCatalogItem {
        id: installed.plugin_id.clone(),
        domain: format!("plugin:{}", installed.plugin_id),
        name: declaration
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| installed.plugin_id.clone()),
        description: declaration
            .as_ref()
            .map(|d| d.summary.clone())
            .unwrap_or_default(),
        category: declaration
            .as_ref()
            .map(|d| match d.category.as_str() {
                "foundation" => "foundation".to_string(),
                _ => "business".to_string(),
            })
            .unwrap_or_else(|| "business".to_string()),
        version: declaration
            .as_ref()
            .map(|d| d.version.clone())
            .unwrap_or_else(|| installed.version.clone()),
        views: vec!["default".to_string()],
        permissions: declaration
            .as_ref()
            .map(|d| d.permissions.clone())
            .unwrap_or_default(),
        // 支持空间：声明文件缓存优先，缺省回落安装时落库的 supportedSpaces
        // （侧载插件无声明缓存，取包内 manifest.json 解析值）
        supported_spaces: declaration
            .and_then(|d| d.supported_spaces)
            .or_else(|| installed.supported_spaces.clone()),
        package: super::catalog::PluginCatalogPackage {
            update_manifest_url: String::new(),
            signature_url: String::new(),
            package_name: String::new(),
            install_command: String::new(),
        },
    }
}

// ------------------------------------------------------------------
// 单测：id 解析矩阵 / URL 模板 / 镜像展开顺序 / id 一致性 / 双源交叉
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// mock 抓取器：键存在 → Ok(Some)；值以 "@ERR" 开头 → Err；缺键 → Ok(None)（404）。
    struct MapFetcher {
        map: BTreeMap<String, String>,
    }

    impl RepoFetcher for MapFetcher {
        fn fetch_text(&self, url: &str, _max_bytes: u64) -> Result<Option<String>, String> {
            match self.map.get(url) {
                Some(text) if text.starts_with("@ERR") => Err(text.clone()),
                Some(text) => Ok(Some(text.clone())),
                None => Ok(None),
            }
        }

        fn fetch_bytes(&self, url: &str, _max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
            match self.map.get(url) {
                Some(text) if text.starts_with("@ERR") => Err(text.clone()),
                Some(text) => Ok(Some(text.clone().into_bytes())),
                None => Ok(None),
            }
        }
    }

    fn fetcher_of(pairs: &[(&str, &str)]) -> MapFetcher {
        MapFetcher {
            map: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    // ---------- id 解析矩阵（规格 §1） ----------

    #[test]
    fn repo_id_parse_matrix() {
        // 正：三 host + 规范化（小写 / 去 scheme / 去尾斜杠 / 去 .git）
        let id = RepoId::parse("HTTPS://GitHub.com/Owner/Repo.git/").unwrap();
        assert_eq!(id.normalized(), "github.com/owner/repo");
        assert_eq!(RepoId::parse("gitlab.com/o/r").unwrap().normalized(), "gitlab.com/o/r");
        assert_eq!(RepoId::parse("gitee.com/o/r").unwrap().normalized(), "gitee.com/o/r");
        assert_eq!(
            RepoId::parse("github.com/o/r").unwrap(),
            RepoId::parse(" github.com/O/R/ ").unwrap()
        );
        // 正：monorepo 子路径
        let mono = RepoId::parse("github.com/owner/repo/plugins/todo").unwrap();
        assert_eq!(mono.normalized(), "github.com/owner/repo/plugins/todo");
        assert_eq!(mono.path, vec!["plugins".to_string(), "todo".to_string()]);
        // 反：host 白名单 / 段数 / 非法字符 / 穿越 / 空
        for bad in [
            "example.com/o/r",
            "github.com/owner",
            "github.com/owner/",
            "github.com//repo",
            "github.com/owner/re po",
            "github.com/owner/repo/../x",
            "github.com/owner/repo/./x",
            "github.com/owner/re%20po",
            "github.com/owner/repo?tab=readme",
            "",
            "   ",
        ] {
            assert_eq!(
                RepoId::parse(bad).unwrap_err(),
                format!("Repo plugin id invalid: {bad}")
            );
        }
        // 反：段长与总长超限
        assert!(RepoId::parse(&format!("github.com/{}/r", "a".repeat(101))).is_err());
        assert!(RepoId::parse(&format!("github.com/o/r/{}", "a".repeat(65))).is_err());
        assert!(RepoId::parse(&format!("github.com/o/{}", "r".repeat(300))).is_err());
        // 反：sub-path 超 8 段
        assert!(RepoId::parse("github.com/o/r/a/b/c/d/e/f/g/h/i").is_err());
    }

    // ---------- URL 模板（规格 §3.1） ----------

    #[test]
    fn url_templates_match_spec() {
        let github = RepoId::parse("github.com/acme/todo").unwrap();
        assert_eq!(
            declaration_origin_urls(&github),
            vec![
                "https://github.com/acme/todo/releases/latest/download/spark-plugin.json",
                "https://raw.githubusercontent.com/acme/todo/HEAD/spark-plugin.json",
            ]
        );
        let gitlab = RepoId::parse("gitlab.com/acme/todo/plugins/x").unwrap();
        assert_eq!(
            declaration_origin_urls(&gitlab),
            vec![
                "https://gitlab.com/acme/todo/-/releases/permalink/latest/downloads/x-spark-plugin.json",
                "https://gitlab.com/acme/todo/-/raw/main/plugins/x/spark-plugin.json",
                "https://gitlab.com/acme/todo/-/raw/master/plugins/x/spark-plugin.json",
            ]
        );
        let gitee = RepoId::parse("gitee.com/acme/todo").unwrap();
        assert_eq!(
            declaration_origin_urls(&gitee),
            vec![
                "https://gitee.com/acme/todo/releases/latest/download/spark-plugin.json",
                "https://gitee.com/acme/todo/raw/main/spark-plugin.json",
                "https://gitee.com/acme/todo/raw/master/spark-plugin.json",
            ]
        );
        // release 资产
        assert_eq!(
            release_asset_origin_url(&github, "v0.2.0", "a.spkg"),
            "https://github.com/acme/todo/releases/download/v0.2.0/a.spkg"
        );
        assert_eq!(
            release_asset_origin_url(&gitlab, "x-v0.2.0", "a.spkg"),
            "https://gitlab.com/acme/todo/-/releases/x-v0.2.0/downloads/a.spkg"
        );
        assert_eq!(
            release_asset_origin_url(&gitee, "v0.2.0", "a.spkg"),
            "https://gitee.com/acme/todo/releases/download/v0.2.0/a.spkg"
        );
    }

    // ---------- 镜像展开顺序（规格 §3.2/§3.3） ----------

    #[test]
    fn mirror_expansion_order() {
        let github = RepoId::parse("github.com/acme/todo").unwrap();
        let sources = declaration_sources(&github);
        let urls: Vec<&str> = sources.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://github.com/acme/todo/releases/latest/download/spark-plugin.json",
                "https://raw.githubusercontent.com/acme/todo/HEAD/spark-plugin.json",
                "https://mirror.ghproxy.com/https://github.com/acme/todo/releases/latest/download/spark-plugin.json",
                "https://gh-proxy.com/https://github.com/acme/todo/releases/latest/download/spark-plugin.json",
                "https://mirror.ghproxy.com/https://raw.githubusercontent.com/acme/todo/HEAD/spark-plugin.json",
                "https://gh-proxy.com/https://raw.githubusercontent.com/acme/todo/HEAD/spark-plugin.json",
            ]
        );
        // 族标记：origin 两个，公共镜像四个
        assert_eq!(sources.iter().filter(|(f, _)| *f == SourceFamily::Origin).count(), 2);
        assert_eq!(sources.iter().filter(|(f, _)| *f == SourceFamily::Public).count(), 4);
        // 非 github 无公共镜像
        assert_eq!(declaration_sources(&RepoId::parse("gitee.com/a/b").unwrap()).len(), 3);
        // release 资产源：origin → 声明 mirrors（sub-path 沿用原 id）→ 公共镜像
        let asset_sources = release_asset_sources(
            &github,
            &["gitee.com/mirror/todo".to_string(), "github.com/mirror/todo".to_string()],
            "v1.0.0",
            "m.json",
        );
        let urls: Vec<&str> = asset_sources.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://github.com/acme/todo/releases/download/v1.0.0/m.json",
                "https://gitee.com/mirror/todo/releases/download/v1.0.0/m.json",
                "https://github.com/mirror/todo/releases/download/v1.0.0/m.json",
                "https://mirror.ghproxy.com/https://github.com/acme/todo/releases/download/v1.0.0/m.json",
                "https://gh-proxy.com/https://github.com/acme/todo/releases/download/v1.0.0/m.json",
            ]
        );
    }

    // ---------- 派生命名（规格 §2.2） ----------

    #[test]
    fn derive_release_names_matrix() {
        let id = RepoId::parse("github.com/acme/todo").unwrap();
        let declaration = SparkPluginDeclaration {
            id: id.normalized(),
            name: "待办".to_string(),
            icon: String::new(),
            summary: "s".to_string(),
            category: "business".to_string(),
            version: "0.2.0".to_string(),
            release_asset_pattern: "spark-plugin-todo-<version>.spkg".to_string(),
            permissions: vec![],
            mirrors: vec![],
            supported_spaces: None,
            sdk_version: "1.0.0".to_string(),
        };
        assert_eq!(
            derive_release_names(&declaration, &id),
            (
                "v0.2.0".to_string(),
                "spark-plugin-todo-0.2.0.spkg".to_string(),
                "spark-plugin-todo-manifest.json".to_string(),
                "spark-plugin-todo-manifest.sig".to_string(),
            )
        );
        // monorepo：tag 加子路径末段前缀
        let mono = RepoId::parse("github.com/acme/plugins/todo").unwrap();
        assert_eq!(derive_release_names(&declaration, &mono).0, "todo-v0.2.0");
    }

    // ---------- id 一致性校验（规格 §4.1） ----------

    fn declaration_text(id: &str, version: &str) -> String {
        serde_json::json!({
            "id": id,
            "name": "待办",
            "icon": "",
            "summary": "测试插件",
            "category": "business",
            "version": version,
            "releaseAssetPattern": "spark-plugin-todo-<version>.spkg",
            "permissions": ["org:sync"],
            "mirrors": [],
            "sdkVersion": "1.0.0"
        })
        .to_string()
    }

    #[test]
    fn declaration_validation_and_id_consistency() {
        let id = RepoId::parse("github.com/acme/todo").unwrap();
        // 正：声明 id 可带 scheme/大小写，规范化后一致即可
        let ok =
            validate_declaration(&declaration_text("HTTPS://GitHub.com/ACME/todo", "0.2.0"), &id).unwrap();
        assert_eq!(ok.name, "待办");
        // 反：id 不一致（信任锚核心拒绝路径）
        assert_eq!(
            validate_declaration(&declaration_text("github.com/evil/todo", "0.2.0"), &id).unwrap_err(),
            "Repo plugin declaration id mismatch: expected github.com/acme/todo, got github.com/evil/todo"
        );
        // 反：声明 id 本身非法也归 mismatch
        assert_eq!(
            validate_declaration(&declaration_text("not a repo", "0.2.0"), &id).unwrap_err(),
            "Repo plugin declaration id mismatch: expected github.com/acme/todo, got not a repo"
        );
        // 反：超 64 KiB / 坏 JSON / 坏 releaseAssetPattern / 坏镜像条目
        let oversized = format!(
            "{}{}",
            declaration_text("github.com/acme/todo", "0.2.0"),
            " ".repeat(64 * 1024)
        );
        assert!(validate_declaration(&oversized, &id).unwrap_err().contains("exceeds 64 KiB"));
        assert!(validate_declaration("{ not json", &id)
            .unwrap_err()
            .starts_with("Repo plugin declaration invalid: github.com/acme/todo:"));
        let bad_pattern = declaration_text("github.com/acme/todo", "0.2.0").replace(".spkg", ".zip");
        assert!(validate_declaration(&bad_pattern, &id)
            .unwrap_err()
            .contains("releaseAssetPattern"));
        let bad_mirror = declaration_text("github.com/acme/todo", "0.2.0")
            .replace("\"mirrors\":[]", "\"mirrors\":[\"example.com/x/y\"]");
        assert!(validate_declaration(&bad_mirror, &id).unwrap_err().contains("mirrors"));
    }

    // ---------- supportedSpaces 校验（规格 §2.1：可选；声明时非空且只含 personal/org） ----------

    #[test]
    fn declaration_supported_spaces_validation() {
        let id = RepoId::parse("github.com/acme/todo").unwrap();
        let with_spaces = |spaces: serde_json::Value| {
            let mut obj: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&declaration_text("github.com/acme/todo", "0.2.0")).unwrap();
            obj.insert("supportedSpaces".to_string(), spaces);
            serde_json::Value::Object(obj).to_string()
        };
        // 正：缺省 = None（前端按 ["org"] 处理）/ personal / org / 两者
        assert_eq!(
            validate_declaration(&declaration_text("github.com/acme/todo", "0.2.0"), &id)
                .unwrap()
                .supported_spaces,
            None
        );
        for spaces in [
            serde_json::json!(["personal"]),
            serde_json::json!(["org"]),
            serde_json::json!(["personal", "org"]),
        ] {
            assert!(validate_declaration(&with_spaces(spaces), &id).is_ok());
        }
        // 反：空数组 / 非法值
        for bad in [serde_json::json!([]), serde_json::json!(["personal", "team"])] {
            assert!(validate_declaration(&with_spaces(bad), &id)
                .unwrap_err()
                .contains("supportedSpaces"));
        }
        // 重复值去重（保持首次出现顺序）
        assert_eq!(
            validate_declaration(&with_spaces(serde_json::json!(["org", "org", "personal"])), &id)
                .unwrap()
                .supported_spaces,
            Some(vec!["org".to_string(), "personal".to_string()])
        );
    }

    // ---------- 字段形状校验（规格 §2.1：version semver / icon 三形态） ----------

    #[test]
    fn declaration_field_shape_matrix() {
        let id = RepoId::parse("github.com/acme/todo").unwrap();
        let invalid_prefix = "Repo plugin declaration invalid: github.com/acme/todo:";
        // version：两段 / 非数字段 / 空 → E_DECL_INVALID
        for bad_version in ["0.2", "0.2.x", "", "0.2.0.0"] {
            let text = declaration_text("github.com/acme/todo", bad_version);
            let err = validate_declaration(&text, &id).unwrap_err();
            assert!(err.starts_with(invalid_prefix), "version {bad_version}: {err}");
            assert!(err.contains("version"), "version {bad_version}: {err}");
        }
        // version 正：预发布后缀与 +build 元数据
        for ok_version in ["0.2.0-rc.1", "0.2.0+build.5"] {
            let text = declaration_text("github.com/acme/todo", ok_version);
            assert!(
                validate_declaration(&text, &id).is_ok(),
                "version {ok_version} should pass"
            );
        }
        // icon：三形态（空 / data: / https）外一律拒
        let with_icon = |icon: &str| {
            declaration_text("github.com/acme/todo", "0.2.0")
                .replace("\"icon\":\"\"", &format!("\"icon\":\"{icon}\""))
        };
        assert!(validate_declaration(&with_icon("http://evil.com/x.png"), &id)
            .unwrap_err()
            .contains("icon"));
        assert!(validate_declaration(&with_icon("ftp://evil.com/x.png"), &id)
            .unwrap_err()
            .contains("icon"));
        assert!(validate_declaration(&with_icon("data:image/png;base64,AA=="), &id).is_ok());
        assert!(validate_declaration(&with_icon("https://cdn.example.com/x.png"), &id).is_ok());
    }

    // ---------- 双源交叉（规格 §3.4） ----------

    #[test]
    fn cross_check_rules() {
        let id = RepoId::parse("github.com/acme/todo").unwrap();
        let sources = declaration_sources(&id);
        let release_url = "https://github.com/acme/todo/releases/latest/download/spark-plugin.json";
        let raw_url = "https://raw.githubusercontent.com/acme/todo/HEAD/spark-plugin.json";
        let proxy_url =
            "https://mirror.ghproxy.com/https://github.com/acme/todo/releases/latest/download/spark-plugin.json";

        // 正：两族源一致 → 通过（异族交叉实际发生）
        let fetcher = fetcher_of(&[(release_url, "A"), (proxy_url, "A")]);
        assert_eq!(
            fetch_with_cross_check(&fetcher, &sources, true, u64::MAX, "MISMATCH", "FETCH"),
            Ok("A".to_string())
        );
        // 正：仅 origin 可达 → 单源降级允许
        let fetcher = fetcher_of(&[(release_url, "A")]);
        assert_eq!(
            fetch_with_cross_check(&fetcher, &sources, true, u64::MAX, "MISMATCH", "FETCH"),
            Ok("A".to_string())
        );
        // 反：异族交叉不一致 → 拒绝
        let fetcher = fetcher_of(&[(release_url, "A"), (raw_url, "A"), (proxy_url, "B")]);
        assert_eq!(
            fetch_with_cross_check(&fetcher, &sources, true, u64::MAX, "MISMATCH", "FETCH"),
            Err("MISMATCH".to_string())
        );
        // 反：仅公共镜像可达 → 声明文件不允许镜像单源
        let fetcher = fetcher_of(&[(proxy_url, "A")]);
        assert_eq!(
            fetch_with_cross_check(&fetcher, &sources, true, u64::MAX, "MISMATCH", "FETCH"),
            Err("FETCH".to_string())
        );
        // 反：全部不可达（404 或错误）→ 取失败
        let fetcher = fetcher_of(&[(release_url, "@ERR reset")]);
        assert_eq!(
            fetch_with_cross_check(&fetcher, &sources, true, u64::MAX, "MISMATCH", "FETCH"),
            Err("FETCH".to_string())
        );
        let fetcher = fetcher_of(&[]);
        assert_eq!(
            fetch_with_cross_check(&fetcher, &sources, true, u64::MAX, "MISMATCH", "FETCH"),
            Err("FETCH".to_string())
        );
    }

    #[test]
    fn fetch_first_semantics() {
        let sources = vec![
            (SourceFamily::Origin, "u1".to_string()),
            (SourceFamily::Origin, "u2".to_string()),
            (SourceFamily::Public, "u3".to_string()),
        ];
        // 404 跳过取下一个
        let fetcher = fetcher_of(&[("u2", "hit")]);
        assert_eq!(fetch_first(&fetcher, &sources, u64::MAX), Ok(Some("hit".to_string())));
        // 全 404 → Ok(None)（资产不存在语义，签名可选层依赖）
        let fetcher = fetcher_of(&[]);
        assert_eq!(fetch_first(&fetcher, &sources, u64::MAX), Ok(None));
        // 全错误（无一 404）→ Err
        let fetcher = fetcher_of(&[("u1", "@ERR a"), ("u2", "@ERR b"), ("u3", "@ERR c")]);
        assert_eq!(fetch_first(&fetcher, &sources, u64::MAX), Err("@ERR c".to_string()));
    }
}
