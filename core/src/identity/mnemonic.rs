//! BIP39 助记词：中文简体词表生成 24 词（256 位熵）；
//! 恢复兼容 `chinese_simplified` 与 `english` 两词表（按输入形态探测）。

use bip39::{Language, Mnemonic};
use rand::Rng;

use super::error::{IdentityError, Result};

/// BIP39 passphrase（固定字符串）。
pub const BIP39_PASSPHRASE: &str = "Polykey";
/// 熵位数：256 位 → 24 词。
pub const ENTROPY_BITS: usize = 256;

/// 支持的助记词词表。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wordlist {
    /// 中文简体（生成默认）。
    ChineseSimplified,
    /// 英文（v1 遗产，仅恢复）。
    English,
}

impl Wordlist {
    fn language(self) -> Language {
        match self {
            Wordlist::ChineseSimplified => Language::SimplifiedChinese,
            Wordlist::English => Language::English,
        }
    }

    /// 词表标识字符串（与规格/向量中的 `wordlist` 字段一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Wordlist::ChineseSimplified => "chinese_simplified",
            Wordlist::English => "english",
        }
    }
}

/// 解析后的助记词：规范化词序列 + 词表 + 派生种子。
#[derive(Clone, Debug)]
pub struct ParsedMnemonic {
    /// 空格分隔的规范化助记词字符串。
    pub mnemonic: String,
    /// 探测到的词表。
    pub wordlist: Wordlist,
    /// BIP39 种子（64 字节），passphrase 固定为 `Polykey`。
    pub seed: [u8; 64],
}

/// 生成 24 词中文简体助记词（256 位熵）。
pub fn generate_mnemonic() -> Result<String> {
    let mut entropy = [0u8; ENTROPY_BITS / 8];
    rand::rng().fill_bytes(&mut entropy);
    let m = Mnemonic::from_entropy_in(Language::SimplifiedChinese, &entropy)
        .map_err(|e| IdentityError::Crypto(format!("entropy to mnemonic: {e}")))?;
    Ok(join_words(&m))
}

/// 逐词对照全部可恢复词表（中文简体 + 英文），返回不在任何词表中的词下标
/// （TS `findInvalidMnemonicWords`，root-mnemonic-check 供 UI 高亮错字）。
pub fn find_invalid_mnemonic_words(words: &[String]) -> Vec<usize> {
    let lists = [
        Language::SimplifiedChinese.word_list(),
        Language::English.word_list(),
    ];
    words
        .iter()
        .enumerate()
        .filter(|(_, word)| !lists.iter().any(|list| list.contains(&word.as_str())))
        .map(|(index, _)| index)
        .collect()
}

/// 解析助记词，按 `chinese_simplified` → `english` 顺序探测词表。
///
/// 校验词表归属与校验和；种子使用固定 passphrase `Polykey`。
pub fn parse_mnemonic(input: &str) -> Result<ParsedMnemonic> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(IdentityError::InvalidMnemonic("empty mnemonic".into()));
    }
    for wordlist in [Wordlist::ChineseSimplified, Wordlist::English] {
        if let Ok(m) = Mnemonic::parse_in_normalized(wordlist.language(), trimmed) {
            return Ok(ParsedMnemonic {
                mnemonic: join_words(&m),
                wordlist,
                seed: m.to_seed(BIP39_PASSPHRASE),
            });
        }
    }
    Err(IdentityError::InvalidMnemonic(
        "not a valid chinese_simplified or english mnemonic".into(),
    ))
}

fn join_words(m: &Mnemonic) -> String {
    m.words().collect::<Vec<_>>().join(" ")
}
