//! 确定性模糊匹配（community-affairs §10 决策 4）：同一查询对任何节点的
//! 同一索引返回同一结果——打分只依赖条目字段（大小写不敏感子串命中，可选
//! 拼写容错按有界编辑距离），排序键为 (score 降序, affairId 升序)，与
//! 插入/到达顺序、节点无关。纯函数：不碰存储，时间不参与打分。

use super::local_index::IndexEntry;

/// 单查询结果上限（affair-metadata §8 查询信封语义）。
pub const MATCH_LIMIT_MAX: usize = 50;

/// 搜索查询（查询信封 payload 的 search 分支）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// 匹配文本（空白切词；空文本不匹配任何条目）。
    pub text: String,
    /// 期望条数（超出 MATCH_LIMIT_MAX 截断）。
    pub limit: usize,
    /// 区域过滤（精确等于；None = 不过滤）。
    pub region: Option<String>,
    /// 标签过滤（条目须包含全部所给标签）。
    pub tags: Vec<String>,
    /// 拼写容错：词条对标题/标签在子串未命中时再按有界编辑距离比对
    /// （阈值只依赖词长，见 [`fuzzy_threshold`]；false = 仅子串精确）。
    pub fuzzy: bool,
    /// 时间过滤：条目 updatedAt 下界（含；None = 不限）。
    pub updated_after: Option<i64>,
    /// 时间过滤：条目 updatedAt 上界（含；None = 不限）。
    pub updated_before: Option<i64>,
    /// 验证状态过滤：只要已对本地日志复算通过的条目。
    pub verified_only: bool,
    /// 健康信号过滤：affair 级异议数上限（含）。需本地日志推导健康信号，
    /// 无健康信号的条目被排除（run_search 生效，见 query.rs）。
    pub max_objections: Option<u64>,
    /// 健康信号过滤：最近链上活动时间下界（含；同上需健康信号）。
    pub active_since_ms: Option<i64>,
    /// 健康信号过滤：最近活跃窗口活跃身份数下限（含；同上需健康信号）。
    pub min_recent_active: Option<u64>,
}

impl SearchQuery {
    /// 最小构造：纯文本 + 条数，无过滤无容错（测试与简单调用方用）。
    pub fn plain(text: &str, limit: usize) -> Self {
        Self {
            text: text.to_string(),
            limit,
            region: None,
            tags: Vec::new(),
            fuzzy: false,
            updated_after: None,
            updated_before: None,
            verified_only: false,
            max_objections: None,
            active_since_ms: None,
            min_recent_active: None,
        }
    }

    /// 是否携带健康信号过滤（任一设置即需要 run_search 逐条推导健康）。
    pub fn has_health_filter(&self) -> bool {
        self.max_objections.is_some()
            || self.active_since_ms.is_some()
            || self.min_recent_active.is_some()
    }
}

/// 命中条目 + 得分。
#[derive(Clone, Debug, PartialEq)]
pub struct MatchHit {
    pub entry: IndexEntry,
    pub score: u64,
}

/// 词条：小写、空白切分、去重（保序）。CJK 无空格文本整体作为一个词条
/// （子串语义，中英同口径）。
fn tokenize(text: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for token in text.split_whitespace() {
        let token = token.to_lowercase();
        if !token.is_empty() && !seen.contains(&token) {
            seen.push(token);
        }
    }
    seen
}

/// 拼写容错阈值（只依赖查询词自身字符数：≤3 不容错，4–7 容 1，≥8 容 2）——
/// 短词容错噪声大（「车位」≠「单位」），阈值确定性保证任何节点口径一致。
fn fuzzy_threshold(token_chars: usize) -> usize {
    match token_chars {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

/// 有界 Levenshtein 编辑距离（字符粒度；任一行最小值超过 max 即提前返回
/// None）。纯函数，结果与调用上下文无关。
fn edit_distance_within(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(cur[j] + 1).min(prev[j + 1] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[b.len()] <= max).then_some(prev[b.len()])
}

/// 词条对字段文本的容错命中：先按空白切词逐词比对；无空格文本（CJK 标题/
/// 标签整体是一个「词」）再按长度 ∈ [len-d, len+d] 的字符滑窗比对，覆盖
/// 增删字符场景。field 须已小写化（与调用方口径一致）。
fn fuzzy_hit(token: &[char], field: &str, max: usize) -> bool {
    if max == 0 || token.is_empty() {
        return false;
    }
    for word in field.split_whitespace() {
        let word_chars: Vec<char> = word.chars().collect();
        if edit_distance_within(token, &word_chars, max).is_some() {
            return true;
        }
    }
    let field_chars: Vec<char> = field.chars().collect();
    let min_len = token.len().saturating_sub(max).max(1);
    let max_len = (token.len() + max).min(field_chars.len());
    for len in min_len..=max_len {
        for start in 0..=field_chars.len() - len {
            if edit_distance_within(token, &field_chars[start..start + len], max).is_some() {
                return true;
            }
        }
    }
    false
}

/// 打分：每个词条在 title/tags/summary 子串命中分别 +3/+2/+1；开启拼写
/// 容错时，title/tags 子串未命中再按编辑距离比对，容错命中得分 = 精确
/// 命中 -1（title +2、tags +1；summary 为长自由文本，容错噪声大，不参与
/// 容错——1-1=0 等价于不计分）。同字段同词条只计一次（出现次数不放大
/// ——防堆词操纵，§10 决策 4 的确定性口径）。
fn score_tokens(tokens: &[String], fuzzy: bool, entry: &IndexEntry) -> u64 {
    let title = entry.title.to_lowercase();
    let summary = entry.summary.to_lowercase();
    let tags: Vec<String> = entry.tags.iter().map(|t| t.to_lowercase()).collect();
    let mut score = 0u64;
    for token in tokens {
        if title.contains(token.as_str()) {
            score += 3;
        } else if fuzzy {
            let token_chars: Vec<char> = token.chars().collect();
            if fuzzy_hit(&token_chars, &title, fuzzy_threshold(token_chars.len())) {
                score += 2;
            }
        }
        if tags.iter().any(|tag| tag.contains(token.as_str())) {
            score += 2;
        } else if fuzzy {
            let token_chars: Vec<char> = token.chars().collect();
            let threshold = fuzzy_threshold(token_chars.len());
            if tags.iter().any(|tag| fuzzy_hit(&token_chars, tag, threshold)) {
                score += 1;
            }
        }
        if summary.contains(token.as_str()) {
            score += 1;
        }
    }
    score
}

/// 确定性搜索：region/tags/时间/验证状态过滤 → 打分 >0 → (score 降序,
/// affairId 升序) → 截断。对同一 entries 集合的任何排列输入，输出逐字节
/// 一致。健康信号过滤不在本层（需存储推导），由 query::run_search 在
/// 打分截断前套用（见 SearchQuery::has_health_filter）。
pub fn search(entries: &[IndexEntry], query: &SearchQuery) -> Vec<MatchHit> {
    let tokens = tokenize(&query.text);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<MatchHit> = entries
        .iter()
        .filter(|entry| {
            query
                .region
                .as_ref()
                .map_or(true, |region| entry.region.as_ref() == Some(region))
        })
        .filter(|entry| query.tags.iter().all(|tag| entry.tags.contains(tag)))
        .filter(|entry| {
            query
                .updated_after
                .map_or(true, |after| entry.updated_at >= after)
        })
        .filter(|entry| {
            query
                .updated_before
                .map_or(true, |before| entry.updated_at <= before)
        })
        .filter(|entry| !query.verified_only || entry.verified)
        .map(|entry| MatchHit {
            score: score_tokens(&tokens, query.fuzzy, entry),
            entry: entry.clone(),
        })
        .filter(|hit| hit.score > 0)
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.entry.affair_id.cmp(&b.entry.affair_id))
    });
    hits.truncate(query.limit.min(MATCH_LIMIT_MAX));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        id: &str,
        title: &str,
        summary: &str,
        tags: &[&str],
        region: Option<&str>,
    ) -> IndexEntry {
        IndexEntry {
            affair_id: id.repeat(4).chars().take(64).collect::<String>(),
            title: title.to_string(),
            summary: summary.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            region: region.map(ToString::to_string),
            meta_seq: 0,
            basis_op_hash: id.repeat(64),
            verified: false,
            updated_at: 1,
        }
    }

    fn corpus() -> Vec<IndexEntry> {
        vec![
            entry(
                "b1",
                "业委会选举",
                "候选人提名与表决",
                &["hoa", "region:110105"],
                Some("110105"),
            ),
            entry("a1", "社区花园改造", "绿化与步道", &["garden"], None),
            entry(
                "c1",
                "停车管理办法",
                "业委会 drafted 选举规则",
                &["parking", "hoa"],
                None,
            ),
        ]
    }

    #[test]
    fn title_scores_higher_than_summary() {
        let hits = search(
            &corpus(),
            &SearchQuery {
                text: "选举".to_string(),
                limit: 10,
                ..SearchQuery::plain("", 0)
            },
        );
        // b1 标题命中（3）> c1 简介命中（1）
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entry.title, "业委会选举");
        assert_eq!(hits[0].score, 3);
        assert_eq!(hits[1].score, 1);
    }

    #[test]
    fn same_input_any_order_same_output() {
        let mut shuffled = corpus();
        shuffled.reverse();
        shuffled.swap(0, 2);
        let query = SearchQuery {
            text: "业委会 hoa".to_string(),
            limit: 50,
            ..SearchQuery::plain("", 0)
        };
        let a = search(&corpus(), &query);
        let b = search(&shuffled, &query);
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    #[test]
    fn tie_breaks_by_affair_id_ascending() {
        let entries = vec![
            entry("z9", " same ", "", &[], None),
            entry("a9", "same", "", &[], None),
        ];
        let hits = search(
            &entries,
            &SearchQuery {
                text: "same".to_string(),
                limit: 10,
                ..SearchQuery::plain("", 0)
            },
        );
        assert_eq!(hits.len(), 2);
        assert!(hits[0].entry.affair_id < hits[1].entry.affair_id);
    }

    #[test]
    fn region_and_tags_filter_before_scoring() {
        let hits = search(
            &corpus(),
            &SearchQuery {
                text: "业委会".to_string(),
                limit: 10,
                region: Some("110105".to_string()),
                tags: vec!["hoa".to_string()],
                ..SearchQuery::plain("", 0)
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.title, "业委会选举");
    }

    #[test]
    fn empty_text_matches_nothing() {
        assert!(
            search(
                &corpus(),
                &SearchQuery {
                    text: "  ".to_string(),
                    ..SearchQuery::plain("", 0)
                },
            )
            .is_empty()
        );
    }

    #[test]
    fn limit_capped_at_max() {
        let entries: Vec<IndexEntry> = (0..80)
            .map(|i| entry(&format!("{i:02}"), "x", "", &[], None))
            .collect();
        let hits = search(
            &entries,
            &SearchQuery {
                text: "x".to_string(),
                limit: 1000,
                ..SearchQuery::plain("", 0)
            },
        );
        assert_eq!(hits.len(), MATCH_LIMIT_MAX);
    }

    #[test]
    fn fuzzy_tolerates_single_typo() {
        // 「选举例办法」(5 字 → 阈值 1) 对标题片段「选举办法」编辑距离 1
        let entries = vec![entry("a1", "业委会选举办法", "", &[], None)];
        let exact = search(
            &entries,
            &SearchQuery {
                text: "选举例办法".to_string(),
                ..SearchQuery::plain("", 10)
            },
        );
        assert!(exact.is_empty(), "未开容错：错字不命中");
        let fuzzy = search(
            &entries,
            &SearchQuery {
                text: "选举例办法".to_string(),
                fuzzy: true,
                ..SearchQuery::plain("", 10)
            },
        );
        assert_eq!(fuzzy.len(), 1, "容错：编辑距离 1 命中标题");
        assert_eq!(fuzzy[0].score, 2, "容错标题命中得分 = 精确 3 - 1");
    }

    #[test]
    fn fuzzy_threshold_scales_with_token_length() {
        // 词长 ≤3 不容错（阈值 0）
        assert_eq!(fuzzy_threshold(3), 0);
        assert_eq!(fuzzy_threshold(4), 1);
        assert_eq!(fuzzy_threshold(7), 1);
        assert_eq!(fuzzy_threshold(8), 2);
        // 短词错字即使开容错也不命中（「车管位」非子串，且 3 字词条阈值 0）
        let entries = vec![entry("a1", "车位管理", "", &[], None)];
        let hits = search(
            &entries,
            &SearchQuery {
                text: "车管位".to_string(),
                fuzzy: true,
                ..SearchQuery::plain("", 10)
            },
        );
        assert!(hits.is_empty(), "3 字词条不容错");
    }

    #[test]
    fn fuzzy_tag_hit_scores_one() {
        let entries = vec![entry("a1", "无关标题", "", &["parking"], None)];
        let hits = search(
            &entries,
            &SearchQuery {
                text: "parkng".to_string(), // 6 字符 → 阈值 1，缺一个字母
                fuzzy: true,
                ..SearchQuery::plain("", 10)
            },
        );
        assert_eq!(hits.len(), 1, "标签容错命中");
        assert_eq!(hits[0].score, 1, "容错标签命中得分 = 精确 2 - 1");
    }

    #[test]
    fn fuzzy_deterministic_across_input_order() {
        let mut shuffled = corpus();
        shuffled.reverse();
        let query = SearchQuery {
            text: "业委会 hoa".to_string(),
            fuzzy: true,
            ..SearchQuery::plain("", 50)
        };
        assert_eq!(search(&corpus(), &query), search(&shuffled, &query));
    }

    #[test]
    fn updated_time_filters_apply_before_scoring() {
        let mut old = entry("a1", "停车管理办法", "", &[], None);
        old.updated_at = 100;
        let mut new = entry("b1", "停车位规划", "", &[], None);
        new.updated_at = 200;
        let entries = vec![old, new];
        let hits = search(
            &entries,
            &SearchQuery {
                text: "停车".to_string(),
                updated_after: Some(150),
                ..SearchQuery::plain("", 10)
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.title, "停车位规划");
        let hits = search(
            &entries,
            &SearchQuery {
                text: "停车".to_string(),
                updated_before: Some(150),
                ..SearchQuery::plain("", 10)
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.title, "停车管理办法");
        // 区间两端皆含
        let hits = search(
            &entries,
            &SearchQuery {
                text: "停车".to_string(),
                updated_after: Some(100),
                updated_before: Some(200),
                ..SearchQuery::plain("", 10)
            },
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn verified_only_filters_unverified() {
        let mut verified = entry("a1", "停车管理办法", "", &[], None);
        verified.verified = true;
        let unverified = entry("b1", "停车位规划", "", &[], None);
        let entries = vec![verified, unverified];
        let hits = search(
            &entries,
            &SearchQuery {
                text: "停车".to_string(),
                verified_only: true,
                ..SearchQuery::plain("", 10)
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.title, "停车管理办法");
    }
}
