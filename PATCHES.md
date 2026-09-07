# PATCHES（transportd/v0.159.0 相对 upstream rust-v0.159.0）

本文件是 fork 的唯一修改面清单（TLS-CTD-001 §10.6 / FR-006）。每一行
都必须可映射到一个需求；修改面只允许四类：新增 binary crate、metadata
adapter、raw response adapter、显式 proxy route adapter。未列出文件一律
与 upstream `rust-v0.159.0`（687a119f0fcaace47e1f1abcc77cec6c813fd6da）
一致。

| # | 文件 | 变更 | 目的 | 需求 |
|---|---|---|---|---|
| P-001 | `codex-rs/codex-transportd/*` | 新增 crate（UDS server、health/info、installation bootstrap、封闭 operation、官方 adapter、client pool、WS relay） | sidecar 本体 | T-002/T-003/T-005/T-009 |
| P-002 | `codex-rs/Cargo.toml` | workspace members 增加 `codex-transportd` | 纳入 locked workspace | T-002 |
| P-003 | `codex-rs/Cargo.lock` | 增加 `codex-transportd` 包节点；本地包版本标签由 cargo 归一化 0.0.0→0.159.0（依赖图不变，官方 lock 本身在 cargo 1.95 下已 stale） | clean lockfile 构建（`--locked` 可复现） | T-002 |
| P-004 | `.github/workflows/transportd.yml` | CI locked build/test/clippy + artifact digest | build-proof CI | T-002 |
| P-005 | `UPSTREAM_BASE.md` / `PATCHES.md` / `codex-rs/codex-transportd/BUILD_PROOF.md` | fork 冻结证据 | CP-002 gate | T-002 |
| P-006 | `codex-rs/core/src/responses_metadata.rs` | 新增 `CodexResponsesMetadata::try_for_transport`（UUIDv7/window 校验窄构造器）与 `TransportMetadataError`；`client_metadata`/`compatibility_headers` 转 pub | metadata adapter（FR-009/FR-010） | T-005 |
| P-007 | `codex-rs/codex-api/src/endpoint/responses.rs` | 新增 `ResponsesClient::stream_raw`（复用 EndpointSession/auth/compression/retry，返回原始 status/headers/bytes stream） | raw response adapter（FR-011） | T-005 |
| P-008 | `codex-rs/codex-api/src/endpoint/compact.rs` | 新增 `CompactClient::execute_raw`（原始 status/headers/body） | raw response adapter（FR-012） | T-005/T-007 |
| P-009 | `codex-rs/codex-api/src/endpoint/models.rs` | 新增 `ModelsClient::execute_raw` | raw response adapter（FR-011） | T-008 |
| P-010 | `codex-rs/codex-api/src/endpoint/search.rs` | 新增 `SearchClient::execute_raw` | raw response adapter（FR-011） | T-008 |
| P-011 | `codex-rs/http-client/src/outbound_proxy.rs` + `lib.rs` | 新增 `build_reqwest_client_for_explicit_route`（Direct 强制 no_proxy；Proxy 精确 pin；失败不回退 direct） | 显式 proxy route adapter（FR-016/FR-017） | T-005 |
| P-012 | `codex-rs/websocket-client/src/lib.rs` | 新增 `WebSocketConnector::connect_with_route`（显式 route，不查系统/环境代理） | 显式 proxy route adapter（FR-016/FR-017） | T-005/T-009 |
| P-013 | `codex-rs/codex-api/src/endpoint/responses_websocket.rs` | 新增 `ResponsesWebsocketClient::connect_with_route`（官方 URL/headers/auth 构造 + 显式 route） | 显式 proxy route adapter（FR-016） | T-009 |
| P-014 | `codex-rs/login/src/auth/{personal_access_token,default_client,manager,mod}.rs` | 可见性 pub 化（`whoami_endpoint`、`PROD_AUTHAPI_BASE_URL`、`create_default_auth_client`、`refresh_token_endpoint` + re-export）；不改任何常量/语义 | 官方组件可见性最小开放：transportd auth 平面复用官方 auth 组件（P1-A/B） | T-012 |
| P-015 | `codex-rs/codex-api/src/endpoint/responses_websocket.rs` | 新增 `ResponsesWebsocketClient::connect_raw_with_route`（与 `connect_with_route` 相同 URL/header/auth/TLS/显式 route 构造，返回原始 `WebSocketConnection` + 握手响应，不包装 WsStream/telemetry） | raw response adapter（F-018：WS relay 复用官方连接逻辑，消除 sidecar 手工 URL/header/auth/connector 复制） | T-009 |

| P-016 | `deploy/build-sidecar.sh` | v8 release tag 由 `codex-rs/Cargo.lock` 的 resolved pin 推导（fail-closed：v8 包数必须恰为 1，否则报错退出）；构建容器内声明 `safe.directory`（否则 root 读 runner 克隆时被 dubious-ownership 拒绝 → build.rs 取 HEAD 失败 → `fork_commit` **静默留空**） | 让构建在 cold v8 cache 与 root 容器下都可复现、且身份不再静默丢失（patch 0017 内） | T-013 |

## 明确不改（禁止面）

- TLS cipher suites / extensions / ALPN / HTTP2 SETTINGS / WebSocket config；
- reqwest / rustls / tokio-tungstenite 依赖版本；
- 官方 retry 次数与 fallback 策略；
- 官方 UA / originator / metadata 常量；
- 不引入 TLS impersonation 库。

## 升级流程

任何 upstream 变更：先更新 `UPSTREAM_BASE.md` 重新固定 tag/commit/lock，
再 clean rebase 本 patch series，重跑 `UPSTREAM_UPGRADE.md` checklist 与
wire gate（T-013）。
