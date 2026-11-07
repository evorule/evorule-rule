//! 机器行权频次配额（per-tenant per-window，内存滑窗实现）。
//!
//! 语义（分级裁决治理矩阵，D3 决策点）：
//! - 每租户在滚动一小时内最多 `max_per_hour` 次机器行权
//!   （`POST /v1/invoke/transition_entry`，服务级自动集成受端）；
//! - 超限 → 受端拒绝（429），调用方降级为人工提案路径入闸——
//!   **fail-visible 不丢弃**：提案物料仍在，仅自动集成通道收敛；
//! - 承载选轻量内存滑窗（usage_records 表无写入路径，schema 零改动）；
//!   服务重启窗口重置=配额计数放宽（一次性），防护主体=节流三层+翻案链，
//!   不影响 fail-visible 语义；
//! - 缺省值内嵌（治理矩阵锁定：扩权=改代码过 PR+裁定留痕；配置面只收不放宽）。

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// 滚动窗口宽度（1 小时）。
pub const QUOTA_WINDOW: Duration = Duration::from_secs(3600);
/// 缺省配额：每租户每小时机器行权次数上限。
pub const DEFAULT_MAX_TRANSITIONS_PER_HOUR: usize = 100;

/// 每租户滑窗配额闸（线程安全；AppState 共享单例）。
#[derive(Debug)]
pub struct TransitionQuota {
    max_per_hour: usize,
    /// tenant_id → 窗口内行权时点队列（队首最旧）
    windows: std::sync::Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl TransitionQuota {
    /// `max_per_hour=0` = 完全禁用配额（显式声明激活，缺省=启用）。
    pub fn new(max_per_hour: usize) -> Self {
        Self {
            max_per_hour,
            windows: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 缺省配额构造（治理矩阵内嵌缺省值）。
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_TRANSITIONS_PER_HOUR)
    }

    /// 配额检查+占位（原子）：窗口内余量充足则记一笔并返回 Ok；
    /// 超限返回 Err（调用方如实上抛 429，不静默）。
    pub fn acquire(&self, tenant_id: &str) -> Result<(), ()> {
        if self.max_per_hour == 0 {
            return Ok(());
        }
        let mut windows = self.windows.lock().unwrap();
        let now = Instant::now();
        let q = windows.entry(tenant_id.to_string()).or_default();
        // 淘汰滑窗外时点
        while let Some(front) = q.front() {
            if now.duration_since(*front) > QUOTA_WINDOW {
                q.pop_front();
            } else {
                break;
            }
        }
        if q.len() >= self.max_per_hour {
            return Err(());
        }
        q.push_back(now);
        Ok(())
    }

    /// 当前窗口内已用额度（观测面，测试与运维探针用）。
    pub fn used(&self, tenant_id: &str) -> usize {
        let windows = self.windows.lock().unwrap();
        windows.get(tenant_id).map(|q| q.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn quota_allows_within_limit_and_blocks_over() {
        let q = TransitionQuota::new(3);
        for _ in 0..3 {
            q.acquire("t1").unwrap();
        }
        assert_eq!(q.used("t1"), 3);
        assert!(q.acquire("t1").is_err(), "超配额必须拒绝");
        // 租户隔离：他租户不受影响
        q.acquire("t2").unwrap();
        assert_eq!(q.used("t2"), 1);
    }

    #[test]
    fn quota_zero_disables() {
        let q = TransitionQuota::new(0);
        for _ in 0..10 {
            q.acquire("t1").unwrap();
        }
    }

    #[test]
    fn quota_window_evicts_stale_entries() {
        let q = TransitionQuota::new(1);
        q.acquire("t1").unwrap();
        assert!(q.acquire("t1").is_err());
        // 手工注入过期时点模拟窗口滚出
        {
            let mut windows = q.windows.lock().unwrap();
            let stale = Instant::now() - QUOTA_WINDOW - Duration::from_secs(1);
            let mut dq = VecDeque::new();
            dq.push_back(stale);
            windows.insert("t1".to_string(), dq);
        }
        q.acquire("t1").unwrap();
        assert_eq!(q.used("t1"), 1);
    }
}
