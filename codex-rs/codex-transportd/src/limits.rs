//! 资源上限（TLS-CTD-001 §6.3 / FR-021 / INV-013）。
//!
//! 所有 buffer/queue/pool/并发均有静态上限；禁止无界 channel。

/// 全局并发 in-flight 请求上限。
pub const MAX_INFLIGHT: usize = 64;

/// UDS accepted connection 上限（包含尚未完成请求头解析的连接）。
pub const MAX_UDS_CONNECTIONS: usize = 128;

/// WS 并发连接上限（1:1）。
pub const MAX_WS_CONNECTIONS: usize = 32;

/// 请求体上限（与 Go 侧限制保持一致）。
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// 请求 header 总字节上限。
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// 单请求 header 数量上限。
pub const MAX_HEADER_COUNT: usize = 64;

/// 账号级 client pool 最大条目数。
pub const CLIENT_POOL_MAX_ENTRIES: usize = 256;

/// 池条目 idle TTL。
pub const CLIENT_POOL_IDLE_TTL_SECONDS: u64 = 300;

/// UDS admission 等待上限。
pub const ADMISSION_WAIT_MS: u64 = 500;

/// UDS dial 超时。
pub const UDS_DIAL_TIMEOUT_MS: u64 = 250;

/// 单 chunk 泵送大小（背压粒度）。
pub const CHUNK_SIZE: usize = 16 * 1024;

/// WS 每方向 frame 队列容量。
pub const WS_FRAME_QUEUE_CAPACITY: usize = 32;

/// 指标 channel 容量（观测系统变慢不能阻塞热路径）。
pub const METRICS_CHANNEL_CAPACITY: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_static_and_positive() {
        // const 断言在编译期验证（避免 clippy constant assertion）。
        const _: () = assert!(MAX_INFLIGHT > 0);
        const _: () = assert!(MAX_UDS_CONNECTIONS > 0);
        const _: () = assert!(MAX_WS_CONNECTIONS > 0);
        const _: () = assert!(MAX_BODY_BYTES > 0);
        const _: () = assert!(MAX_HEADER_BYTES > 0);
        const _: () = assert!(MAX_HEADER_COUNT > 0);
        const _: () = assert!(CLIENT_POOL_MAX_ENTRIES > 0);
        const _: () = assert!(CHUNK_SIZE > 0);
        const _: () = assert!(WS_FRAME_QUEUE_CAPACITY > 0);
    }
}
