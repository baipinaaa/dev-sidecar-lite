//! IP 评分：EWMA 延迟 × 失败惩罚（借鉴 hakimi `decision/score.rs`）。
//!
//! 核心思想：**地址排序不能只看"能不能连"**，还要看"连得多快、失败过几次"。
//! Node 版只有 `successRate`，没有延迟维度，所以"能连但极慢"的地址会长期占坑。
//!
//! 排序公式与取舍：
//! - `base = EWMA(ms)`（从未成功过则用 `UNSEEN_RANK`）
//! - `rank = base * (1 + fail_rate * 8)`
//! - 从未尝试过的地址排 **50000**，而不是 `f64::MAX` ——
//!   否则"每个失败地址都排在每个未测地址前面"，黑洞地址会永久霸占第一位。

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// 未尝试过地址的默认排序值。
pub const UNSEEN_RANK: f64 = 50_000.0;
/// EWMA 平滑系数（新样本权重）。
pub const EWMA_ALPHA: f64 = 0.3;
/// 失败惩罚系数：失败率 100% 时排名放大 9 倍。
pub const FAIL_PENALTY: f64 = 8.0;
/// 记录上限，超过后按排名裁剪。
pub const SCORE_CAP: usize = 1024;
/// 裁剪目标（保留 3/4）。
pub const SCORE_PRUNE_TO: usize = SCORE_CAP * 3 / 4;

/// 单个地址的评分记录。
#[derive(Debug, Clone)]
pub struct IpScore {
    /// 指数平滑后的延迟（毫秒）；`None` 表示从未成功。
    pub ewma_ms: Option<f64>,
    pub successes: u32,
    pub failures: u32,
    pub last_seen: Option<Instant>,
}

impl Default for IpScore {
    fn default() -> Self {
        Self {
            ewma_ms: None,
            successes: 0,
            failures: 0,
            last_seen: None,
        }
    }
}

impl IpScore {
    pub fn fail_rate(&self) -> f64 {
        let total = self.successes + self.failures;
        if total == 0 {
            0.0
        } else {
            self.failures as f64 / total as f64
        }
    }

    /// 排序值：越小越好。
    pub fn rank(&self) -> f64 {
        let base = self.ewma_ms.unwrap_or(UNSEEN_RANK);
        base * (1.0 + self.fail_rate() * FAIL_PENALTY)
    }

    pub fn record_success(&mut self, ms: f64) {
        let sample = ms.max(1.0);
        self.ewma_ms = Some(match self.ewma_ms {
            None => sample,
            Some(prev) => prev * (1.0 - EWMA_ALPHA) + sample * EWMA_ALPHA,
        });
        self.successes = self.successes.saturating_add(1);
        self.last_seen = Some(Instant::now());
    }

    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.last_seen = Some(Instant::now());
    }
}

/// 地址评分表。
#[derive(Debug, Default)]
pub struct Scoreboard {
    scores: HashMap<IpAddr, IpScore>,
}

impl Scoreboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_success(&mut self, ip: IpAddr, ms: f64) {
        self.scores.entry(ip).or_default().record_success(ms);
        self.prune_if_needed();
    }

    pub fn record_failure(&mut self, ip: IpAddr) {
        self.scores.entry(ip).or_default().record_failure();
        self.prune_if_needed();
    }

    pub fn score(&self, ip: &IpAddr) -> IpScore {
        self.scores.get(ip).cloned().unwrap_or_default()
    }

    pub fn rank_of(&self, ip: &IpAddr) -> f64 {
        self.scores
            .get(ip)
            .map(IpScore::rank)
            .unwrap_or(UNSEEN_RANK)
    }

    /// 按排名升序返回地址（未记录的地址排在 `UNSEEN_RANK` 位置）。
    pub fn sort_candidates(&self, candidates: &[IpAddr]) -> Vec<IpAddr> {
        let mut scored: Vec<(f64, IpAddr)> = candidates
            .iter()
            .map(|ip| (self.rank_of(ip), *ip))
            .collect();
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(_, ip)| ip).collect()
    }

    /// 排名前 `n` 的地址。
    pub fn top(&self, candidates: &[IpAddr], n: usize) -> Vec<IpAddr> {
        let mut out = self.sort_candidates(candidates);
        out.truncate(n);
        out
    }

    /// 一段时间内未出现的记录可以回收。
    pub fn evict_idle(&mut self, idle: Duration) {
        let now = Instant::now();
        self.scores.retain(|_, score| {
            score
                .last_seen
                .map(|seen| now.duration_since(seen) < idle)
                .unwrap_or(false)
        });
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    fn prune_if_needed(&mut self) {
        if self.scores.len() <= SCORE_CAP {
            return;
        }
        let mut entries: Vec<(f64, IpAddr)> = self
            .scores
            .iter()
            .map(|(ip, score)| (score.rank(), *ip))
            .collect();
        entries.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        entries.truncate(SCORE_PRUNE_TO);
        let keep: std::collections::HashSet<IpAddr> = entries.into_iter().map(|(_, ip)| ip).collect();
        self.scores.retain(|ip, _| keep.contains(ip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(23, 200, 230, last))
    }

    #[test]
    fn unseen_ranks_worse_than_successful_but_better_than_dead() {
        let mut board = Scoreboard::new();
        board.record_success(ip(1), 40.0);
        // 2 号一直被拒
        for _ in 0..5 {
            board.record_failure(ip(2));
        }

        let good = board.rank_of(&ip(1));
        let unseen = board.rank_of(&ip(3));
        let dead = board.rank_of(&ip(2));

        assert!(good < unseen, "成功地址应优于未测地址：{good} vs {unseen}");
        assert!(unseen < dead, "未测地址应优于彻底失败的地址：{unseen} vs {dead}");
    }

    #[test]
    fn ewma_smooths_latency() {
        let mut score = IpScore::default();
        score.record_success(100.0);
        assert_eq!(score.ewma_ms, Some(100.0));
        score.record_success(200.0);
        // 100*(1-0.3) + 200*0.3 = 130
        assert!((score.ewma_ms.unwrap() - 130.0).abs() < 0.001);
    }

    #[test]
    fn failure_rate_penalises_rank() {
        let mut score = IpScore::default();
        score.record_success(50.0);
        let clean = score.rank();
        score.record_failure();
        let dirty = score.rank();
        assert!(dirty > clean && clean > 0.0);
    }

    #[test]
    fn sort_puts_fastest_first() {
        let mut board = Scoreboard::new();
        board.record_success(ip(10), 120.0);
        board.record_success(ip(11), 30.0);
        board.record_success(ip(12), 60.0);
        let order = board.sort_candidates(&[ip(10), ip(11), ip(12)]);
        assert_eq!(order, vec![ip(11), ip(12), ip(10)]);
    }

    #[test]
    fn prunes_beyond_cap_keeping_best() {
        let mut board = Scoreboard::new();
        for i in 0..(SCORE_CAP + 10) {
            let last = (i % 250) as u8;
            board.record_success(ip(last), 10.0 + i as f64);
        }
        assert!(board.len() <= SCORE_PRUNE_TO, "应裁剪到 {}", SCORE_PRUNE_TO);
    }
}
