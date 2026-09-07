//! 低基数观测（TLS-CTD-001 §17）。
//!
//! 指标只有 operation/outcome/phase/proxy_kind/route_class 等低基数标签；
//! 禁止 account id、user id、session/thread/window、request id、token
//! revision 原文、proxy host/URL 作为 label（INV-012）。

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::contract::ProxyKind;
use crate::contract::RequestPurpose;
use crate::contract::operation::Operation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 响应 headers 已发但 body 流式阶段以错误/断连结束（H4）。
    StreamError,
    Success,
    LocalError,
    UpstreamError,
    Cancelled,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StreamError => "stream_error",
            Self::Success => "success",
            Self::LocalError => "local_error",
            Self::UpstreamError => "upstream_error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 进程内低基数指标（无外部依赖；观测变慢不阻塞热路径）。
#[derive(Debug, Default)]
pub struct Metrics {
    pub requests_total: LabeledCounter,
    pub time_to_headers_ms: AtomicU64,
    pub stream_duration_ms: AtomicU64,
    pub retries_total: LabeledCounter,
    pub errors_total: LabeledCounter,
    pub inflight: AtomicU64,
    pub queue_depth: AtomicU64,
    pub pool_entries: AtomicU64,
    pub pool_evictions_total: LabeledCounter,
    pub ws_connections: AtomicU64,
    pub ws_frames_total: LabeledCounter,
    pub fallback_total: AtomicU64,
    pub go_egress_total: AtomicU64,
    /// capture 事件因队列满/关闭被丢弃的次数（capture-r1）。
    pub capture_dropped_total: AtomicU64,
    /// capture 因溢出被标记截断的 chunk 次数（capture-r1）。
    pub capture_truncated_total: AtomicU64,
    /// capture writer 落盘失败的次数（capture-r1）。
    pub capture_write_failures_total: AtomicU64,
}

/// `(op, outcome)` 组合计数器。
#[derive(Debug, Default)]
pub struct LabeledCounter {
    buckets: std::sync::Mutex<std::collections::BTreeMap<String, u64>>,
}

impl LabeledCounter {
    pub fn increment(&self, key: impl Into<String>) {
        if let Ok(mut buckets) = self.buckets.lock() {
            *buckets.entry(key.into()).or_default() += 1;
        }
    }

    /// 按 delta 递增（retry 计数一次写入 attempts-1，避免逐次 +1 的
    /// 锁竞争；delta=0 是 no-op）。
    pub fn increment_by(&self, key: impl Into<String>, delta: u64) {
        if delta == 0 {
            return;
        }
        if let Ok(mut buckets) = self.buckets.lock() {
            *buckets.entry(key.into()).or_default() += delta;
        }
    }

    pub fn snapshot(&self) -> std::collections::BTreeMap<String, u64> {
        self.buckets.lock().map(|b| b.clone()).unwrap_or_default()
    }
}

/// retry 观测的终态 outcome（R-OBS-001；有界：每 operation 两值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOutcome {
    /// 终态 2xx（含成功前的 retry）。
    SuccessAfterRetry,
    /// retryable 状态耗尽预算后的终态非 2xx。
    RetryExhausted,
}

impl RetryOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SuccessAfterRetry => "success_after_retry",
            Self::RetryExhausted => "retry_exhausted",
        }
    }
}

/// R-OBS-001：真实 retry 次数进入 `retries_total{operation,outcome}`。
///
/// - `attempts <= 1`（无 retry）零增量；
/// - 有 retry 时按终态 outcome 一次性记 `attempts - 1`，单一记账点，
///   绝不双计；
/// - label 只有封闭 `Operation` × 两值 outcome，基数有界（INV-012）。
pub fn observe_retries(
    metrics: &Metrics,
    operation: Operation,
    attempts: u32,
    terminal_success: bool,
) {
    if attempts <= 1 {
        return;
    }
    let outcome = if terminal_success {
        RetryOutcome::SuccessAfterRetry
    } else {
        RetryOutcome::RetryExhausted
    };
    let retries = u64::from(attempts - 1);
    metrics.retries_total.increment_by(
        format!("{}|{}", operation.metric_label(), outcome.as_str()),
        retries,
    );
}

/// 一次请求的观测句柄。
#[derive(Debug, Clone, Copy)]
pub struct RequestObservation {
    pub operation: Operation,
    pub proxy_kind: ProxyKind,
    pub purpose: RequestPurpose,
}

pub fn observe_request_start(metrics: &Metrics, obs: RequestObservation) {
    let _ = obs;
    metrics.inflight.fetch_add(1, Ordering::Relaxed);
    metrics
        .requests_total
        .increment(format!("{}|{}", obs.operation.metric_label(), "started"))
}

pub fn observe_request_end(metrics: &Metrics, obs: RequestObservation, outcome: Outcome) {
    metrics.inflight.fetch_sub(1, Ordering::Relaxed);
    metrics.requests_total.increment(format!(
        "{}|{}",
        obs.operation.metric_label(),
        outcome.as_str()
    ));
}

pub fn observe_error(metrics: &Metrics, obs: RequestObservation, class: &str, phase: &str) {
    metrics.errors_total.increment(format!(
        "{}|{}|{}",
        obs.operation.metric_label(),
        class,
        phase
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_low_cardinality_and_bounded() {
        let metrics = Metrics::default();
        let obs = RequestObservation {
            operation: Operation::Responses,
            proxy_kind: ProxyKind::Direct,
            purpose: RequestPurpose::Request,
        };
        observe_request_start(&metrics, obs);
        observe_request_end(&metrics, obs, Outcome::Success);
        let snapshot = metrics.requests_total.snapshot();
        assert_eq!(snapshot.get("responses|started"), Some(&1));
        assert_eq!(snapshot.get("responses|success"), Some(&1));
    }

    #[test]
    fn fallback_counter_must_always_be_zero() {
        let metrics = Metrics::default();
        assert_eq!(metrics.fallback_total.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.go_egress_total.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn retry_counts_are_exact_single_counted_and_bounded() {
        // R-OBS-001：无 retry 零增量；有 retry 按终态 outcome 精确一次记账。
        let metrics = Metrics::default();
        observe_retries(&metrics, Operation::Responses, 1, true);
        assert!(
            metrics.retries_total.snapshot().is_empty(),
            "无 retry 不得产生 retry 指标"
        );
        observe_retries(&metrics, Operation::Responses, 2, true);
        observe_retries(&metrics, Operation::ResponsesCompact, 5, false);
        assert_eq!(
            metrics.retries_total.snapshot(),
            std::collections::BTreeMap::from([
                ("responses|success_after_retry".to_string(), 1),
                ("responses_compact|retry_exhausted".to_string(), 4),
            ])
        );
    }
}
