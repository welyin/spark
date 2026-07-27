//! SLIP-0010 派生路径解析/格式化单测。

use spark_core::identity::slip10::{format_derivation_path, parse_derivation_path};

#[test]
fn path_roundtrip() {
    let path = "m/44'/607'/0'/0'/0'";
    let indices = parse_derivation_path(path).unwrap();
    assert_eq!(indices, vec![44, 607, 0, 0, 0]);
    assert_eq!(format_derivation_path(&indices), path);
}

#[test]
fn rejects_non_hardened() {
    assert!(parse_derivation_path("m/44'/607'/0").is_err());
    assert!(parse_derivation_path("m/44'/x'/0'").is_err());
    assert!(parse_derivation_path("m/").is_err());
}
