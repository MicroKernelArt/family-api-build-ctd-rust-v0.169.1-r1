//! 官方 client factory 适配（TLS-CTD-001 §10.6 / FR-005 / FR-010）。
//!
//! 所有 upstream client 都由官方组件构造：
//! - UA/originator 来自 `codex_login::default_client` 官方 generator；
//! - Cloudflare cookie store 来自 `codex_http_client`；
//! - 显式账号级 proxy route 来自 fork patch 的
//!   `build_reqwest_client_for_explicit_route`；
//! - auth 使用官方 `BearerAuthProvider`（Authorization + ChatGPT-Account-ID）。
//!
//! 本模块不修改任何 TLS/H2/WS 配置或依赖版本。

use std::sync::Arc;
use std::time::Duration;

use codex_api::Provider;
use codex_api::RetryConfig;
use codex_api::SharedAuthProvider;
use codex_client::ReqwestTransport;
use codex_http_client::ClientRouteClass;
use codex_http_client::OutboundProxyRoute;
use codex_http_client::build_reqwest_client_for_explicit_route;
use codex_http_client::with_chatgpt_cloudflare_cookie_store;
use codex_model_provider::BearerAuthProvider;
use http::HeaderMap;

use crate::contract::ProxyKind;
use crate::contract::error::ErrorClass;
use crate::contract::error::ErrorPhase;
use crate::contract::error::LocalError;
use crate::contract::operation::Operation;
use crate::redaction::SecretString;
use crate::transport::client_pool::ClientPool;
use crate::transport::proxy::ProxyDigest;
use crate::transport::proxy::ResolvedProxyRoute;
use crate::transport::proxy::{self};

/// 官方 ChatGPT Codex 端点 base（`/backend-api/codex`）。
pub const CHATGPT_CODEX_BASE_URL: &str = codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
/// WHAM 端点 base（`/backend-api`）。
pub const CHATGPT_WHAM_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// 官方 retry 配置（与 `ModelProviderInfo::to_api_provider` 相同）。
fn official_retry() -> RetryConfig {
    RetryConfig {
        max_attempts: 4,
        base_delay: Duration::from_millis(200),
        retry_429: false,
        retry_5xx: true,
        retry_transport: true,
    }
}

/// 测试目标 override（TLS-CTD-001 §15.2）：仅在 `test-upstream` cargo
/// feature 下编译；production CI 必须证明该 feature 关闭。base URL 只
/// 覆盖 Provider 目标，不改变封闭 operation 的 path/method 映射。
fn test_upstream_base_url(default: &str, env_key: &str) -> String {
    let _ = env_key; // 非 test-upstream 构建时未使用
    #[cfg(feature = "test-upstream")]
    {
        if let Ok(url) = std::env::var(env_key)
            && !url.is_empty()
        {
            return url;
        }
    }
    #[allow(unreachable_code)]
    default.to_string()
}

/// 官方 ChatGPT provider（codex base）。
pub fn chatgpt_provider() -> Provider {
    Provider {
        name: "chatgpt".to_string(),
        base_url: test_upstream_base_url(CHATGPT_CODEX_BASE_URL, "CTD_TEST_UPSTREAM_BASE_URL"),
        query_params: None,
        headers: official_version_header(),
        retry: official_retry(),
        stream_idle_timeout: Duration::from_secs(300),
    }
}

/// v3（hookpoint=v3）专用 provider：与 [`chatgpt_provider`] 同 base/retry/
/// idle timeout，但 **headers 为空**。
///
/// ★ 必需：`EndpointSession::make_request` 用
/// `provider.build_request(...)` 起步，再 `req.headers.extend(extra_headers)`。
/// `HeaderMap::extend` 对**同名键是替换**（`http` 1.4.0 `header/map.rs`
/// `Extend<(Option<HeaderName>, T)>` 的 `Entry::Occupied` 分支 `e.insert(val)`），
/// 但对 **provider 独有、Go 未交付的头则原样保留**。若沿用
/// [`chatgpt_provider`]，provider 自带的 `version`（`CARGO_PKG_VERSION`）会在
/// Go 未交付 `version` 时被 ctd 自行补上——**v3 下 ctd 不得做任何 header
/// 决策**，那会变成 ctd 谎报自身构建版本。
///
/// 因此 v3 的头集**完全由 Go 决定**，provider 不得贡献任何默认头。
pub fn chatgpt_provider_bare() -> Provider {
    Provider {
        name: "chatgpt".to_string(),
        base_url: test_upstream_base_url(CHATGPT_CODEX_BASE_URL, "CTD_TEST_UPSTREAM_BASE_URL"),
        query_params: None,
        headers: HeaderMap::new(),
        retry: official_retry(),
        stream_idle_timeout: Duration::from_secs(300),
    }
}

/// 官方 WHAM provider（usage/quota）。
pub fn wham_provider() -> Provider {
    Provider {
        name: "chatgpt-wham".to_string(),
        base_url: test_upstream_base_url(CHATGPT_WHAM_BASE_URL, "CTD_TEST_WHAM_BASE_URL"),
        query_params: None,
        headers: official_version_header(),
        retry: official_retry(),
        stream_idle_timeout: Duration::from_secs(300),
    }
}

/// 官方 `version` header（与 `create_openai_provider` 相同）。
fn official_version_header() -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = http::HeaderValue::from_str(env!("CARGO_PKG_VERSION")) {
        headers.insert("version", value);
    }
    headers
}

/// 解析后的账号级 route（带 digest）。
pub struct RouteHandle {
    pub route: ResolvedProxyRoute,
    pub digest: ProxyDigest,
}

/// 解析 X-CTD proxy 输入为显式官方 route + 不可逆 digest。
pub fn resolve_route_handle(
    kind: ProxyKind,
    proxy_url: Option<&SecretString>,
) -> Result<RouteHandle, LocalError> {
    let route = proxy::resolve_route(kind, proxy_url)?;
    let digest = match &route {
        ResolvedProxyRoute::Direct => ProxyDigest::new("direct"),
        ResolvedProxyRoute::Proxy { url } => proxy::proxy_digest(url.as_str()),
    };
    Ok(RouteHandle { route, digest })
}

/// 官方 route-aware client 构建（显式 route；direct 忽略环境代理）。
pub fn build_official_client(
    route: &OutboundProxyRoute,
    route_class: ClientRouteClass,
) -> Result<reqwest::Client, LocalError> {
    let builder = reqwest::Client::builder()
        // F-002：封闭 operation 拒绝跨 host 重定向（任何 3xx 原样返回，
        // 第二跳零 socket；redirect 语义由 Go/caller 显式处理）。
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(codex_login::default_client::default_headers());
    let builder = with_chatgpt_cloudflare_cookie_store(builder);
    build_reqwest_client_for_explicit_route(builder, route, route_class).map_err(|err| {
        LocalError::new(
            "client_build_failed",
            ErrorClass::Transport,
            ErrorPhase::Connect,
            true,
            err.to_string(),
        )
    })
}

/// 从池获取官方 client（账号/凭据/proxy 隔离）。
pub async fn pooled_official_client(
    pool: &ClientPool,
    account_key: &str,
    credential_revision: &str,
    handle: &RouteHandle,
    operation: Operation,
) -> Result<reqwest::Client, LocalError> {
    let official_route = proxy::to_official_route(&handle.route);
    let route_class = operation.route_class();
    let key = crate::transport::client_pool::ClientPoolKey::new(
        account_key,
        credential_revision,
        handle.digest.clone(),
        route_class_label(route_class),
    );
    pool.get_or_insert_with(key, || {
        build_official_client(&official_route, route_class).map_err(|e| e.message)
    })
    .await
    .map_err(|message| {
        LocalError::new(
            "client_build_failed",
            ErrorClass::Transport,
            ErrorPhase::Connect,
            true,
            message,
        )
    })
}

fn route_class_label(class: ClientRouteClass) -> &'static str {
    match class {
        ClientRouteClass::Api => "api",
        ClientRouteClass::WebSocket => "wss",
        ClientRouteClass::Auth => "auth",
        ClientRouteClass::Other => "other",
    }
}

/// 官方 Bearer auth（Authorization + ChatGPT-Account-ID）。
pub fn official_auth(ctx: &crate::contract::RequestContext) -> SharedAuthProvider {
    Arc::new(BearerAuthProvider {
        token: Some(ctx.access_token.as_str().to_string()),
        account_id: ctx
            .chatgpt_account_id
            .as_ref()
            .map(|id| id.as_str().to_string()),
        is_fedramp_account: false,
    })
}

/// ReqwestTransport 包装（复用池化 client）。
pub fn transport_for(client: reqwest::Client) -> ReqwestTransport {
    ReqwestTransport::new(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_provider_uses_official_base_and_retry() {
        let provider = chatgpt_provider();
        assert_eq!(provider.base_url, "https://chatgpt.com/backend-api/codex");
        assert_eq!(provider.retry.max_attempts, 4);
        assert!(provider.retry.retry_5xx);
        assert!(!provider.retry.retry_429);
    }

    #[test]
    fn wham_provider_uses_backend_api_root() {
        let provider = wham_provider();
        assert_eq!(provider.base_url, "https://chatgpt.com/backend-api");
    }

    #[test]
    fn bare_provider_contributes_no_default_headers() {
        // ★ v3（hookpoint=v3）必需：EndpointSession::make_request 用
        // provider.build_request 起步后 headers.extend(extra_headers)。
        // extend 对同名键是**替换**，但 provider 独有、Go 未交付的头会原样
        // 保留 ⇒ 非 bare provider 会让 ctd 在 v3 下自行补 version（谎报自身
        // 构建版本）。bare provider 保证 v3 的头集完全由 Go 决定。
        let bare = chatgpt_provider_bare();
        assert!(bare.headers.is_empty(), "bare provider 不得携带任何默认头");
        // 除默认头外与官方 provider 逐字段一致（同一 base/retry/idle）。
        let official = chatgpt_provider();
        assert_eq!(bare.base_url, official.base_url);
        assert_eq!(bare.name, official.name);
        assert_eq!(bare.retry.max_attempts, official.retry.max_attempts);
        assert_eq!(bare.stream_idle_timeout, official.stream_idle_timeout);
        // 官方 provider 恰好贡献 version（这正是 v3 必须绕开的那个头）。
        assert_eq!(official.headers.len(), 1);
        assert!(official.headers.contains_key("version"));
    }

    #[test]
    fn v3_header_set_is_go_owned_not_provider_augmented() {
        // 复现"若不换 bare provider 会发生什么"：Go 未交付 version 时，
        // 官方 provider 会替 ctd 补一个 CARGO_PKG_VERSION。
        let go_headers_without_version = http::HeaderMap::new();

        let mut with_official = chatgpt_provider().headers.clone();
        with_official.extend(go_headers_without_version.clone());
        assert_eq!(
            with_official.get("version").unwrap(),
            env!("CARGO_PKG_VERSION"),
            "非 bare provider 会在 Go 未交付时自补 version"
        );

        let mut with_bare = chatgpt_provider_bare().headers.clone();
        with_bare.extend(go_headers_without_version);
        assert!(with_bare.is_empty(), "bare provider 下头集完全由 Go 决定");

        // 同名键的语义是**替换**（不是 append）：Go 交付的 version 覆盖 provider 的。
        let mut go_headers = http::HeaderMap::new();
        go_headers.insert("version", http::HeaderValue::from_static("0.159.2"));
        let mut merged = chatgpt_provider().headers.clone();
        merged.extend(go_headers.clone());
        assert_eq!(merged.get_all("version").iter().count(), 1);
        assert_eq!(merged.get("version").unwrap(), "0.159.2");

        let mut merged = chatgpt_provider_bare().headers.clone();
        merged.extend(go_headers);
        assert_eq!(merged.get("version").unwrap(), "0.159.2");
    }

    #[test]
    fn version_header_comes_from_package_version() {
        let headers = official_version_header();
        assert_eq!(headers.get("version").unwrap(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn route_digest_is_stable_and_reversible_never() {
        let handle = resolve_route_handle(
            ProxyKind::Http,
            Some(&SecretString::from("http://user:pass@p:8080")),
        )
        .expect("route");
        assert_eq!(
            handle.digest.as_str(),
            proxy::proxy_digest("http://user:pass@p:8080").as_str()
        );
        assert_ne!(handle.digest.as_str(), "http://user:pass@p:8080");
    }

    #[test]
    fn direct_route_digest_is_marker() {
        let handle = resolve_route_handle(ProxyKind::Direct, None).expect("direct");
        assert_eq!(handle.digest.as_str(), "direct");
    }
}
