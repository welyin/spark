//! 通用限流器（peer-exchange 60s / org-recovery 30s / node-announce 见 announce.rs）。

use std::collections::HashMap;

/// 同一请求方两次服务的最小间隔限流器。
pub struct MinIntervalRateLimiter {
    min_interval_ms: i64,
    last_served_at: HashMap<String, i64>,
}

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
        self.last_served_at.insert(requester.to_string(), now_ms);
        false
    }
}
