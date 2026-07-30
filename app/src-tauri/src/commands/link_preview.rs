//! 链接预览抓取（ui-messages.md §6，F5）：发送方本地抓取 URL 元数据
//! （OG/Twitter Card 优先，回退 `<title>`/`<meta name="description">`），
//! 随消息携带投递；接收方只展示不访问。
//!
//! 安全边界（本文件头注释即边界声明，§6.4「不抓取内网」）：
//! - HTTP 出口只在壳层，不进 core；抓取失败/超时/非 HTML/内网一律返回
//!   `None`（静默降级为无预览，不阻塞消息发送）。
//! - SSRF 守卫只做**字面量防护**：scheme 限 http/https；hostname 为 IP
//!   字面量时拒绝私有/回环/链路本地/未指定段与 100.64/10 CGNAT 共享段；
//!   hostname 为 localhost（含尾点与 *.localhost）/.local/.internal 拒绝。
//!   **重定向逐跳手工跟随、每跳重新过守卫**（自动跟随不复查目标，可被
//!   302 跳进内网）。**不做 DNS 解析后 IP 检查，不防 DNS rebinding**（域名
//!   解析到内网 IP 的场景不在本期防护范围）。
//! - 资源上限：connect 3s / 每跳请求 5s（重定向逐跳手工跟随，最多 4 个请求
//!   串行，最坏约 20s——挂起的服务器会延迟该条消息落库，属有意的诚实口径）/
//!   重定向 ≤3 跳 / 响应最多读 512 KiB / 只扫 `<head>`（粗略截到 `</head>`）。

use std::io::Read;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;
use std::time::Duration;

use spark_core::message::LinkPreview;

/// 响应体读取上限（512 KiB）。
const MAX_BODY_BYTES: u64 = 512 * 1024;
/// 解析出的字段入库前预截断（内核 `sanitize_link_preview` 仍会再收敛一次，
/// 这里先截避免把超长 meta content 原样带出壳层）。
const MAX_FIELD_CHARS: usize = 512;

/// 全局 blocking Client（懒建）：connect 3s、单请求 5s（每跳语义，非全程）、
/// **重定向手工跟随**（`Policy::none()`——自动跟随不会对跳转目标重新过
/// SSRF 守卫）、UA 标 Spark。
fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("Spark/{} link-preview", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    })
}

/// 从文本提取首个 http(s) URL（与前端正则 `https?://[^\s]+` 同口径：
/// 小写 scheme，取到首个空白字符为止）。
pub(crate) fn extract_first_url(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i..].starts_with(b"http://") || bytes[i..].starts_with(b"https://") {
            // 命中处必为 ASCII 字符边界，切片安全
            let rest = &text[i..];
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// hostname 是否为内网/本机字面量（SSRF 守卫，仅字面量判定，见文件头边界）。
pub(crate) fn is_private_host(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    // 尾点归一：WHATWG 保留尾点（`localhost.` ≠ `localhost`），DNS 仍解析回本机
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return true;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        // 127/8 回环、10/8 与 172.16/12 与 192.168/16 私网、169.254/16 链路本地、0.0.0.0
        if ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() {
            return true;
        }
        // 100.64.0.0/10 CGNAT 共享段（std is_private 不含；Tailscale 等尾网服务
        // 正是这段，对本项目用户等同内网字面量）
        let o = ip.octets();
        return o[0] == 100 && (o[1] & 0xC0) == 64;
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        // ::1 回环、fc00::/7 唯一本地、fe80::/10 链路本地、:: 未指定（与 v4 的
        // 0.0.0.0 同口径——多数系统 connect 未指定地址按回环处理）；
        // IPv4 映射地址（::ffff:a.b.c.d）按内嵌 IPv4 同口径判定
        if let Some(v4) = ip.to_ipv4_mapped() {
            return is_private_host(&v4.to_string());
        }
        return ip.is_loopback()
            || ip.is_unique_local()
            || ip.is_unicast_link_local()
            || ip.is_unspecified();
    }
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
}

/// `<head>` 解析出的元数据（字段均已做实体解码；缺省为空串）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeadMetadata {
    pub title: String,
    pub description: String,
    pub site_name: String,
}

/// 按字符数截断。
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// HTML 实体最小解码（`&amp; &lt; &gt; &quot; &#39; &nbsp;`）：
/// 单次扫描命中即替换、未命中原样保留——避免多轮替换造成二次解码。
fn decode_entities(s: &str) -> String {
    const ENTITIES: [(&str, &str); 6] = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", "\u{a0}"),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        match ENTITIES.iter().find(|(entity, _)| rest.starts_with(entity)) {
            Some((entity, decoded)) => {
                out.push_str(decoded);
                rest = &rest[entity.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 解析 `<meta ...>` 标签的属性表（属性名小写归一；双引号/单引号/无引号
/// 三种写法都收，属性顺序无关——`property` 在前在后都能解）。
fn parse_attrs(tag: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut rest = tag;
    while !rest.is_empty() {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else { break };
        let name = rest[..eq].trim().to_ascii_lowercase();
        rest = rest[eq + 1..].trim_start();
        let (value, advance) = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let inner = &rest[quote.len_utf8()..];
                match inner.find(quote) {
                    Some(end) => (&inner[..end], quote.len_utf8() + end + quote.len_utf8()),
                    None => (inner, rest.len()),
                }
            }
            _ => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                (&rest[..end], end)
            }
        };
        if !name.is_empty() {
            attrs.push((name, decode_entities(value)));
        }
        rest = &rest[advance..];
    }
    attrs
}

/// 从属性表取值（首个匹配；忽略空值）。
fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

/// 解析 `<head>` 片段：og:*（`property=`）优先，twitter:*（`name=`）次之，
/// 回退 `<title>` 与 `<meta name="description">`；siteName 仅取 og:site_name
/// （缺省空串，前端回退域名白名单）。无 `<head>` 容错：扫不到就当空页。
pub(crate) fn parse_head_metadata(head: &str) -> HeadMetadata {
    let lower = head.to_ascii_lowercase();
    let mut og = HeadMetadata::default();
    let mut twitter = HeadMetadata::default();
    let mut plain_desc = String::new();
    let mut offset = 0;
    // meta 标签逐个扫：lower 定位、原串取值（保留大小写；偏移对两串一致——
    // to_ascii_lowercase 只动 ASCII 字节，不改字节布局）
    while let Some(pos) = lower[offset..].find("<meta") {
        let tag_start = offset + pos + "<meta".len();
        let tag_end = lower[tag_start..]
            .find('>')
            .map(|end| tag_start + end)
            .unwrap_or(lower.len());
        let tag = &head[tag_start..tag_end];
        let attrs = parse_attrs(tag);
        offset = tag_end;
        let Some(content) = attr(&attrs, "content") else { continue };
        let key = attr(&attrs, "property").or_else(|| attr(&attrs, "name"));
        let Some(key) = key.map(|k| k.to_ascii_lowercase()) else { continue };
        let set_once = |slot: &mut String| {
            if slot.is_empty() {
                *slot = content.trim().to_string();
            }
        };
        match key.as_str() {
            "og:title" => set_once(&mut og.title),
            "og:description" => set_once(&mut og.description),
            "og:site_name" => set_once(&mut og.site_name),
            "twitter:title" => set_once(&mut twitter.title),
            "twitter:description" => set_once(&mut twitter.description),
            "description" => {
                if plain_desc.is_empty() {
                    plain_desc = content.trim().to_string();
                }
            }
            _ => {}
        }
    }
    // <title> 回退（实体解码 + 去空白）
    let mut html_title = String::new();
    if let Some(open) = lower.find("<title") {
        let after_open = &lower[open..];
        if let Some(gt) = after_open.find('>') {
            let body_start = open + gt + 1;
            let body_end = lower[body_start..]
                .find("</title")
                .map(|end| body_start + end)
                .unwrap_or(head.len());
            html_title = decode_entities(&head[body_start..body_end]).trim().to_string();
        }
    }
    HeadMetadata {
        title: [og.title, twitter.title, html_title]
            .into_iter()
            .find(|t| !t.is_empty())
            .unwrap_or_default(),
        description: [og.description, twitter.description, plain_desc]
            .into_iter()
            .find(|d| !d.is_empty())
            .unwrap_or_default(),
        site_name: og.site_name,
    }
}

/// 重定向跟随上限（跳）。
const MAX_REDIRECTS: usize = 3;

/// URL 是否允许抓取：scheme 限 http/https 且 host 过 SSRF 守卫。
fn url_is_fetchable(url: &reqwest::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !is_private_host(host))
}

/// 逐跳手工跟随重定向发 GET（每跳重新过 SSRF 守卫——自动跟随不复查跳转
/// 目标，公网 URL 可被 302 带进内网）；3xx 无 Location、超跳数、任一跳
/// 不过守卫均返回 `None`。
fn send_guarded(mut url: reqwest::Url) -> Option<reqwest::blocking::Response> {
    for _ in 0..=MAX_REDIRECTS {
        if !url_is_fetchable(&url) {
            return None;
        }
        let response = client().get(url.clone()).send().ok()?;
        if !response.status().is_redirection() {
            return Some(response);
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())?;
        url = url.join(location).ok()?;
    }
    None
}

/// 抓取并生成链接预览；失败/超时/非 HTML/内网/无 URL 元数据均返回 `None`
/// （调用方静默降级为无预览发送）。
pub(crate) fn fetch_link_preview(url: &str) -> Option<LinkPreview> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    let response = send_guarded(parsed)?;
    if !response.status().is_success() {
        return None;
    }
    // content-type 缺失按 html 处理；显式非 text/html 跳过
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase());
    if let Some(content_type) = content_type {
        if !content_type.starts_with("text/html") {
            return None;
        }
    }
    // 最多读 512 KiB
    let mut buf = Vec::new();
    response
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    buf.truncate(MAX_BODY_BYTES as usize);
    let html = String::from_utf8_lossy(&buf);
    // 只扫 <head>（粗略截到 </head>；无 </head> 时扫已读到的全部）
    let head_end = html
        .to_ascii_lowercase()
        .find("</head")
        .unwrap_or(html.len());
    let meta = parse_head_metadata(&html[..head_end]);
    // 页面无任何可用元数据时不发卡（避免 title/description 全空的卡片
    // 把前端的诚实占位整体替换掉）
    if meta.title.is_empty() && meta.description.is_empty() {
        return None;
    }
    let domain = host
        .strip_prefix("www.")
        .unwrap_or(&host)
        .to_ascii_lowercase();
    Some(LinkPreview {
        url: url.to_string(),
        title: truncate_chars(&meta.title, MAX_FIELD_CHARS),
        description: truncate_chars(&meta.description, MAX_FIELD_CHARS),
        // siteName 缺省置空串（前端替换时回退占位白名单/域名）
        site_name: truncate_chars(&meta.site_name, MAX_FIELD_CHARS),
        domain,
    })
}

#[cfg(test)]
mod tests;
