//! 通用限流器（peer-exchange 60s / org-recovery 30s / node-announce 见 announce.rs）。

use std::collections::HashMap;

/// 同一请求方两次服务的最小间隔限流器。
pub struct MinIntervalRateLimiter {
    min_interval_ms: i64,
    last_served_at: HashMap<String, i64>,
}

/// `last_served_at` 容量上限：请求方标识对端可控，不设上限内存无界。
const MAX_TRACKED_REQUESTERS: usize = 1024;

impl MinIntervalRateLimiter {
    pub fn new(min_interval_ms: i64) -> Self {
        Self {
            min_interval_ms,
            last_served_at: HashMap::new(),
        }
    }

    /// 命中限流返回 true；未命中则记录本次服务时间。
    pub fn is_rate_limited(&mut self, requester: &str, now_ms: i64) -> bool {
        if let Some(last) = self.last_served_at.get(requester)
            && now_ms - last < self.min_interval_ms
        {
            return true;
        }
        if !self.last_served_at.contains_key(requester)
            && self.last_served_at.len() >= MAX_TRACKED_REQUESTERS
        {
            // 先回收窗口外的过期条目；仍满则整体清空（有界优先于精确，
            // 清空仅短暂放宽限流，不影响正确性）。
            let min_interval = self.min_interval_ms;
            self.last_served_at
                .retain(|_, last| now_ms - *last < min_interval);
            if self.last_served_at.len() >= MAX_TRACKED_REQUESTERS {
                self.last_served_at.clear();
            }
        }
        self.last_served_at.insert(requester.to_string(), now_ms);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_bounded() {
        let mut limiter = MinIntervalRateLimiter::new(60_000);
        for i in 0..(MAX_TRACKED_REQUESTERS * 3) {
            assert!(!limiter.is_rate_limited(&format!("peer-{i}"), 1_000));
        }
        assert!(limiter.last_served_at.len() <= MAX_TRACKED_REQUESTERS);
    }

    #[test]
    fn full_map_purges_expired_entries_first() {
        let mut limiter = MinIntervalRateLimiter::new(1_000);
        for i in 0..MAX_TRACKED_REQUESTERS {
            limiter.is_rate_limited(&format!("peer-{i}"), 1_000);
        }
        // 全部条目已在窗口外：新请求方触发回收而非整体清空/拒绝
        assert!(!limiter.is_rate_limited("new-peer", 1_000 + 1_000));
        assert_eq!(limiter.last_served_at.len(), 1);
    }
}
