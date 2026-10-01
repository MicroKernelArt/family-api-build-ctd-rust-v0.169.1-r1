//! 编译期/运行期固定的 upstream 与构建证据。
//!
//! T-002 (CP-002) 冻结：fork 仓库拓扑、upstream commit、lock hash、
//! toolchain、base image digest 与 build-proof digest。该文件只读，不改
//! 官方 TLS/H2/WS 配置或依赖版本。

/// Release ID（R4-B2：外部 manifest 引用的不可变发布标识；
/// 构建流水线可覆盖为正式 ID，缺省为 dev）。
pub const RELEASE_ID: &str = match option_env!("CODEX_TRANSPORTD_RELEASE_ID") {
    Some(value) => value,
    None => "ctd-rust-v0.159.2-dev",
};

/// Upstream tag（官方 exact-tag fork 基准）。
pub const UPSTREAM_TAG: &str = "rust-v0.159.2";

/// Upstream peeled commit（`git ls-remote refs/tags/rust-v0.159.2^{}`）。
/// 8b9fa496 是 annotated tag 对象；peeled source commit 才是 ff6aec96……
/// （real-lane codexp RF1）。与 Go 侧 /info pin 一致使用 peeled commit。
pub const UPSTREAM_COMMIT: &str = "ff6aec96948b70d94983af2641a6b67c94faeff5";

/// Upstream `codex-rs/Cargo.lock` SHA-256（fork 之前官方锁文件）。
pub const UPSTREAM_LOCK_SHA256: &str =
    "4fab6fe99180e2b8f305bd4b33c1b4bd825e3fa4cdef1045be750843bd3d431e";

/// Rust toolchain（`codex-rs/rust-toolchain.toml`）。
pub const TOOLCHAIN: &str = "1.95.0";

/// 实际运行镜像 base（R8-F4：与 Dockerfile 运行阶段一致——debian
/// trixie-slim，digest 固定；不再报告官方上游的 Ubuntu 引用）。
pub const BASE_IMAGE_REF: &str =
    "debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258";

/// 编译目标三元组（build.rs 注入）。
pub const TARGET: &str = env!("CODEX_TRANSPORTD_TARGET");

/// 编译期注入的 fork commit（构建脚本/CI 传入），缺省为空字符串。
pub const FORK_COMMIT_FROM_ENV: &str = match option_env!("CODEX_TRANSPORTD_FORK_COMMIT") {
    Some(value) => value,
    None => "",
};

/// 编译期注入的 fork Cargo.lock SHA-256。
pub const FORK_LOCK_FROM_ENV: &str = match option_env!("CODEX_TRANSPORTD_FORK_LOCK") {
    Some(value) => value,
    None => "",
};

/// 编译期注入的 build-proof binary digest。
pub const BUILD_PROOF_FROM_ENV: &str = match option_env!("CODEX_TRANSPORTD_BUILD_PROOF_DIGEST") {
    Some(value) => value,
    None => "",
};

/// /info 暴露的 capabilities 列表（INV-015：只宣告已实现并经本地 e2e
/// 验证的 operation；P1 oauth/files/realtime 未过独立 gate 不宣告）。
pub const CAPABILITIES: &[&str] = &[
    "uds",
    "http1",
    "health",
    "info",
    "installation_bootstrap",
    "responses_http",
    "responses_compact",
    "responses_ws_v2",
    "models",
    "alpha_search",
    "usage",
    "quota_credits",
    "quota_consume",
    "auth_whoami",
    "auth_refresh",
    "file_download",
    "attachment_download",
    "file_content_fetch",
    "agent_task_register",
];
