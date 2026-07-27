//! 身份文件资料字段校验单测。

use spark_core::identity::file::{sanitize_profile, validate_avatar, validate_nickname};

#[test]
fn nickname_validation() {
    assert_eq!(validate_nickname("  Alice ").unwrap(), "Alice");
    assert!(validate_nickname("   ").is_err());
    assert!(validate_nickname("").is_err());
    assert!(validate_nickname(&"a".repeat(24)).is_ok());
    assert!(validate_nickname(&"a".repeat(25)).is_err());
    // 按字符数而非字节数：24 个汉字合法
    assert!(validate_nickname(&"汉".repeat(24)).is_ok());
    assert!(validate_nickname(&"汉".repeat(25)).is_err());
}

#[test]
fn avatar_validation() {
    assert!(validate_avatar("data:image/png;base64,iVBORw0KGgo=").is_ok());
    assert!(validate_avatar("https://example.com/a.png").is_err());
    assert!(validate_avatar("data:text/html;base64,AAAA").is_err());
    let big = format!("data:image/png;base64,{}", "A".repeat(300 * 1024));
    assert!(validate_avatar(&big).is_err());
    let edge = format!("data:image/png;base64,{}", "A".repeat(200 * 1024 - 100));
    assert!(validate_avatar(&edge).is_ok());
}

#[test]
fn sanitize_drops_invalid() {
    let (n, a) = sanitize_profile(Some("  Bob "), Some("data:image/png;base64,AAAA"));
    assert_eq!(n.as_deref(), Some("Bob"));
    assert_eq!(a.as_deref(), Some("data:image/png;base64,AAAA"));
    let (n, a) = sanitize_profile(Some(&"x".repeat(99)), Some("not-an-image"));
    assert!(n.is_none());
    assert!(a.is_none());
    let (n, a) = sanitize_profile(None, None);
    assert!(n.is_none() && a.is_none());
}
