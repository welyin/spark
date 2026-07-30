//! 链接预览抓取单测：解析器/SSRF 守卫/URL 提取（不跑真实网络）。

use super::*;

// ---------- URL 提取 ----------

#[test]
fn extract_first_url_matches_frontend_regex() {
    // 与前端 `https?://[^\s]+` 同口径：首个 http(s) URL，取到空白为止
    assert_eq!(
        extract_first_url("看看这个 https://zhihu.com/question/1 怎么样"),
        Some("https://zhihu.com/question/1".to_string())
    );
    assert_eq!(
        extract_first_url("http://a.com/x https://b.com/y"),
        Some("http://a.com/x".to_string())
    );
    assert_eq!(
        extract_first_url("https://a.com/中文路径"),
        Some("https://a.com/中文路径".to_string())
    );
    assert_eq!(extract_first_url("没有链接"), None);
    assert_eq!(extract_first_url("ftp://a.com/x"), None);
    assert_eq!(extract_first_url("短 ht tp://a.com"), None);
}

// ---------- SSRF 守卫 ----------

#[test]
fn is_private_host_rejects_private_ip_literals() {
    // IPv4 私有/回环/链路本地/未指定
    for host in [
        "127.0.0.1",
        "127.1.2.3",
        "10.0.0.1",
        "10.255.255.255",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.1.1",
        "0.0.0.0",
    ] {
        assert!(is_private_host(host), "{host} 应拒绝");
    }
    // IPv6 回环/唯一本地/链路本地 + IPv4 映射 + 未指定（与 v4 的 0.0.0.0 同口径）
    for host in ["::1", "fc00::1", "fd12::1", "fe80::1", "::ffff:192.168.0.1", "::"] {
        assert!(is_private_host(host), "{host} 应拒绝");
    }
    // 100.64.0.0/10 CGNAT 共享段（Tailscale 等尾网），边界内外都要准
    for host in ["100.64.0.1", "100.100.0.1", "100.127.255.254"] {
        assert!(is_private_host(host), "{host} 应拒绝");
    }
    // 本机域名
    for host in ["localhost", "printer.local", "db.internal", "LOCALHOST"] {
        assert!(is_private_host(host), "{host} 应拒绝");
    }
}

#[test]
fn is_private_host_allows_public() {
    for host in [
        "8.8.8.8",
        "172.15.0.1", // 172.16/12 边界之外
        "172.32.0.1",
        "11.0.0.1",
        "100.63.255.255", // CGNAT 边界之外
        "100.128.0.1",
        "zhihu.com",
        "github.com",
        "2606:4700::1",
    ] {
        assert!(!is_private_host(host), "{host} 应放行");
    }
}

// ---------- head 元数据解析 ----------

#[test]
fn parse_prefers_open_graph() {
    let head = r#"
        <meta property="og:title" content="OG 标题">
        <meta property="og:description" content="OG 描述">
        <meta property="og:site_name" content="知乎">
        <meta name="twitter:title" content="Twitter 标题">
        <title>网页标题</title>
    "#;
    let meta = parse_head_metadata(head);
    assert_eq!(meta.title, "OG 标题");
    assert_eq!(meta.description, "OG 描述");
    assert_eq!(meta.site_name, "知乎");
}

#[test]
fn parse_falls_back_to_twitter_then_html() {
    // 无 og → twitter
    let meta = parse_head_metadata(
        r#"<meta name="twitter:title" content="Twitter 标题">
           <meta name="twitter:description" content="Twitter 描述">
           <title>网页标题</title>"#,
    );
    assert_eq!(meta.title, "Twitter 标题");
    assert_eq!(meta.description, "Twitter 描述");
    assert_eq!(meta.site_name, "");

    // 无 og/twitter → <title> 与 meta description
    let meta = parse_head_metadata(
        r#"<meta name="description" content="网页描述"><title>网页标题</title>"#,
    );
    assert_eq!(meta.title, "网页标题");
    assert_eq!(meta.description, "网页描述");
}

#[test]
fn parse_attr_order_and_quotes() {
    // content 在前 property 在后、单引号、大小写混杂都要能解
    let meta = parse_head_metadata(
        r#"<meta content='乱序标题' property="og:title">
           <META NAME="twitter:title" CONTENT="推特标题">"#,
    );
    assert_eq!(meta.title, "乱序标题");
    let meta = parse_head_metadata(r#"<META NAME="twitter:title" CONTENT="推特标题">"#);
    assert_eq!(meta.title, "推特标题");
}

#[test]
fn parse_decodes_entities() {
    let meta = parse_head_metadata(
        r#"<meta property="og:title" content="A &amp; B &lt;x&gt; &quot;q&quot; &#39;s&#39;">
           <title>T &amp; U</title>"#,
    );
    assert_eq!(meta.title, "A & B <x> \"q\" 's'");
    // 不二次解码：&amp;lt; → &lt;（字面），不是 <
    let meta = parse_head_metadata(r#"<meta property="og:title" content="&amp;lt;">"#);
    assert_eq!(meta.title, "&lt;");
}

#[test]
fn parse_tolerates_malformed_head() {
    // 空串 / 无 head / 未闭合标签均容错为空
    assert_eq!(parse_head_metadata(""), HeadMetadata::default());
    assert_eq!(parse_head_metadata("plain text"), HeadMetadata::default());
    let meta = parse_head_metadata(r#"<meta property="og:title" content="未闭合"#);
    assert_eq!(meta.title, "未闭合");
    // content 为空的标签跳过，回退到 title
    let meta = parse_head_metadata(r#"<meta property="og:title" content=""><title>回退</title>"#);
    assert_eq!(meta.title, "回退");
}

// ---------- SSRF 守卫 × URL 归一化组合 ----------

#[test]
fn ssrf_guard_applies_after_url_normalization() {
    // reqwest::Url 解析时会把各种等价写法归一化，守卫必须作用在归一化后的
    // host 上（而非原始字符串），否则下列写法可绕过 IP 字面量判定
    for u in [
        "http://2130706433/", // 127.0.0.1 十进制数值写法
        "http://0x7f000001/", // 127.0.0.1 hex 写法
        "http://127.1/",      // 127.0.0.1 短写
        "http://evil.com@127.0.0.1/",
        "http://[::]:8080/",
        "http://[::1]/",
        "http://100.64.0.1:8080/",
        "http://localhost./",
        "http://foo.localhost/",
        "http://printer.local./",
    ] {
        let parsed = reqwest::Url::parse(u).unwrap();
        assert!(!url_is_fetchable(&parsed), "{u} 应拒绝");
    }
    assert!(url_is_fetchable(
        &reqwest::Url::parse("https://zhihu.com/question/1").unwrap()
    ));
    assert!(!url_is_fetchable(
        &reqwest::Url::parse("ftp://example.com/x").unwrap()
    ));
}

// ---------- 实体解码 ----------

#[test]
fn decode_entities_minimal_set() {
    assert_eq!(decode_entities("&nbsp;x"), "\u{a0}x");
    assert_eq!(decode_entities("未知 &xyz; 原样"), "未知 &xyz; 原样");
    assert_eq!(decode_entities("孤立 & 号"), "孤立 & 号");
}
