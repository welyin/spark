//! Canonical JSON：逐字节复刻 JS `normalizeObject`（desktop/src/main/db/evidence.ts:22-31）。
//!
//! 规则（core/spec/sync-evidence.md §1）：
//! - `undefined` → `"undefined"`（Rust 侧以 `Option<&Value>` 的 `None` 表达）
//! - `null` → `"null"`
//! - 非 object → `JSON.stringify(value)`：数字按 JS `Number::toString`、字符串按 JS 转义
//! - object（含数组，JS `typeof [] === 'object'`）→ key 排序后，**每个值先递归 normalize
//!   再作为字符串值**嵌入（嵌套对象被序列化成 JSON 字符串，而非结构化 canonical JSON）
//!
//! key 序：先按 UTF-16 code unit 字典序（`Object.keys().sort()`），随后 `JSON.stringify`
//! 对整数型 key（canonical array index，< 2^32-1）恒按数值升序前置——净效果为
//! 「整数型 key 数值升序在前，其余 key 按 UTF-16 code unit 字典序」。

use serde_json::Value;

/// `undefined` / `null` / 常规值的统一入口：`None` 表示 JS `undefined`。
pub fn normalize_value(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(v) => normalize_object(v),
    }
}

/// 复刻 JS `normalizeObject(value)`。
pub fn normalize_object(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        // JS 数字均为 f64：经 as_f64 归一后按 JS Number::toString 规则格式化
        Value::Number(n) => js_number_to_string(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => js_json_escape_string(s),
        Value::Array(items) => {
            // 数组落入 object 分支：key 为 "0","1",...，全部为整数型 key，数值序即自然序
            let mut out = String::from("{");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&js_json_escape_string(&i.to_string()));
                out.push(':');
                out.push_str(&js_json_escape_string(&normalize_object(item)));
            }
            out.push('}');
            out
        }
        Value::Object(map) => {
            // Object.keys().sort()：UTF-16 code unit 字典序（对 BMP 字符等价于 Rust
            // 字节序；对代理对（BMP 外字符），UTF-16 lead surrogate 0xD800-0xDBFF <
            // 0xE000，与码点序不同，故显式按 UTF-16 单元比较以逐字节对齐 JS）
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            // JSON.stringify：整数型 key 数值升序前置，其余保持插入序（已排序）
            let (mut int_keys, other_keys): (Vec<(u32, &str)>, Vec<&str>) = (
                keys.iter()
                    .filter_map(|k| canonical_array_index(k).map(|n| (n, *k)))
                    .collect(),
                keys.iter()
                    .filter(|k| canonical_array_index(k).is_none())
                    .copied()
                    .collect(),
            );
            int_keys.sort_by_key(|(n, _)| *n);

            let mut out = String::from("{");
            let mut first = true;
            for key in int_keys.into_iter().map(|(_, k)| k).chain(other_keys) {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&js_json_escape_string(key));
                out.push(':');
                out.push_str(&js_json_escape_string(&normalize_object(&map[key])));
            }
            out.push('}');
            out
        }
    }
}

/// JS canonical array index 判定：十进制数字、无前导零（"0" 本身除外）、< 2^32-1。
///
/// 对应 ECMA `ToString(ToUint32(P)) === P && ToUint32(P) !== 2^32 - 1`。
fn canonical_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    let n: u64 = key.parse().ok()?;
    if n < u64::from(u32::MAX) { Some(n as u32) } else { None }
}

/// JS `JSON.stringify(string)` 的转义规则：`"`、`\`、`\b \f \n \r \t` 短转义、
/// 其余 < 0x20 控制字符 `\u00xx`（小写 hex）；非 ASCII（含 U+2028/U+2029）不转义。
fn js_json_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JS `Number::toString(n)`（即 `JSON.stringify(number)` 的数字格式）。
///
/// 实现：Rust `{}` 对 f64 输出最短往返十进制（无指数），其有效数字与 V8 的
/// 最短表示一致；据此提取 (digits, k, n_exp) 后按 ECMA-262 Number::toString
/// 的分段规则重排：小数点位置在 [1, 21] 内用定点、(-6, 0] 用 `0.000x`，
/// 其余用科学计数法（`1e+21`、`1.5e-7`）。`-0` → `"0"`。
pub fn js_number_to_string(n: f64) -> String {
    if n == 0.0 {
        // 含 -0（JS: String(-0) === "0"）
        return "0".to_string();
    }
    if !n.is_finite() {
        // JSON.stringify(NaN/Infinity) → null；serde_json 不会产生，防御分支
        return "null".to_string();
    }
    let negative = n.is_sign_negative();
    // 最短往返十进制，形如 "123"、"0.001"、"123.45"（无指数）
    let rust = format!("{}", n.abs());
    let (int_part, frac_part) = match rust.split_once('.') {
        Some((i, f)) => (i, f),
        None => (rust.as_str(), ""),
    };
    let combined: String = int_part.chars().chain(frac_part.chars()).collect();
    let first_significant = combined
        .bytes()
        .position(|b| b != b'0')
        .expect("non-zero value has a significant digit");
    let digits = combined[first_significant..].trim_end_matches('0');
    let k = digits.len() as i64;
    let n_exp = int_part.len() as i64 - first_significant as i64;

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if k <= n_exp && n_exp <= 21 {
        // 整数：digits 后补零，无小数点
        out.push_str(digits);
        out.push_str(&"0".repeat((n_exp - k) as usize));
    } else if 0 < n_exp && n_exp <= 21 {
        // 小数点在数字中间
        out.push_str(&digits[..n_exp as usize]);
        out.push('.');
        out.push_str(&digits[n_exp as usize..]);
    } else if -6 < n_exp && n_exp <= 0 {
        // 0.000…digits
        out.push_str("0.");
        out.push_str(&"0".repeat((-n_exp) as usize));
        out.push_str(digits);
    } else {
        // 科学计数法：指数部分带显式符号（正数带 '+'）
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let e = n_exp - 1;
        if e >= 0 {
            out.push('+');
        }
        out.push_str(&e.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_array_index_rules() {
        assert_eq!(canonical_array_index("0"), Some(0));
        assert_eq!(canonical_array_index("4294967294"), Some(4294967294));
        assert_eq!(canonical_array_index("4294967295"), None); // 2^32-1 不算
        assert_eq!(canonical_array_index("4294967296"), None);
        assert_eq!(canonical_array_index("00"), None);
        assert_eq!(canonical_array_index("01"), None);
        assert_eq!(canonical_array_index("+1"), None);
        assert_eq!(canonical_array_index(" 1"), None);
        assert_eq!(canonical_array_index("1.0"), None);
        assert_eq!(canonical_array_index(""), None);
        assert_eq!(canonical_array_index("12345678901"), None); // 11 位
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 来自验收向量，非数学常数 π 的误用
    fn js_number_formats() {
        assert_eq!(js_number_to_string(0.0), "0");
        assert_eq!(js_number_to_string(-0.0), "0");
        assert_eq!(js_number_to_string(42.0), "42");
        assert_eq!(js_number_to_string(-5.0), "-5");
        assert_eq!(js_number_to_string(3.14), "3.14");
        assert_eq!(js_number_to_string(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(js_number_to_string(0.1), "0.1");
        assert_eq!(js_number_to_string(1e21), "1e+21");
        assert_eq!(js_number_to_string(1e20), "100000000000000000000");
        assert_eq!(js_number_to_string(1e-6), "0.000001");
        assert_eq!(js_number_to_string(1e-7), "1e-7");
        assert_eq!(js_number_to_string(1.5e-7), "1.5e-7");
        assert_eq!(js_number_to_string(1e100), "1e+100");
        assert_eq!(js_number_to_string(-1e21), "-1e+21");
        assert_eq!(js_number_to_string(1.7e12), "1700000000000");
        assert_eq!(js_number_to_string(1.2345678901234568e20), "123456789012345680000");
        assert_eq!(js_number_to_string(1.7976931348623157e308), "1.7976931348623157e+308");
        assert_eq!(js_number_to_string(5e-324), "5e-324");
        assert_eq!(js_number_to_string(100.0), "100");
    }

    #[test]
    fn js_string_escapes() {
        assert_eq!(js_json_escape_string("hi"), "\"hi\"");
        assert_eq!(js_json_escape_string("你好"), "\"你好\""); // 非 ASCII 不转义
        assert_eq!(js_json_escape_string("a\nb"), "\"a\\nb\"");
        assert_eq!(js_json_escape_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(js_json_escape_string("\u{0b}"), "\"\\u000b\""); // 小写 hex
        assert_eq!(js_json_escape_string("\u{7f}"), "\"\u{7f}\""); // DEL 不转义
        assert_eq!(js_json_escape_string("\u{2028}\u{2029}"), "\"\u{2028}\u{2029}\"");
    }

    #[test]
    fn nested_object_stringified() {
        // 关键语义：嵌套对象 normalize 后作为字符串值嵌入
        assert_eq!(normalize_object(&json!({"a": {"b": 1}})), "{\"a\":\"{\\\"b\\\":\\\"1\\\"}\"}");
        assert_eq!(normalize_object(&json!({"a": 1})), "{\"a\":\"1\"}");
        assert_eq!(normalize_object(&json!([])), "{}");
        assert_eq!(normalize_object(&json!({})), "{}");
    }

    #[test]
    fn astral_keys_sort_by_utf16() {
        // "😀"(U+1F600, UTF-16 D83D DE00) 在 UTF-16 序中小于 "\u{E000}"；码点序则相反
        let v = json!({"\u{E000}": 1, "😀": 2});
        let out = normalize_object(&v);
        assert!(out.find("😀").unwrap() < out.find('\u{E000}').unwrap());
    }
}
