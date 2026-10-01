//! Responses HTTP/SSE 原始适配（TLS-CTD-001 §12.1 / FR-011）。
//!
//! 复用官方 `ResponsesClient::stream_raw`（fork patch），返回上游
//! status/headers/byte stream；SSE 不 parse 再重组，不全量缓冲。

use bytes::Bytes;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;

use crate::contract::error::ErrorClass;
use crate::contract::error::ErrorPhase;
use crate::contract::error::LocalError;
use crate::contract::operation::Operation;
use crate::contract::RawUpstreamResponse;
use crate::contract::RequestContext;
use crate::transport::metadata;
use crate::transport::official_adapter;

/// Responses 专用 session headers（与官方 `build_session_headers` 一致）。
pub fn session_headers(ctx: &RequestContext) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(session) = &ctx.session {
        if let Ok(value) = HeaderValue::from_str(&session.session_id) {
            headers.insert("session-id", value);
        }
        if let Ok(value) = HeaderValue::from_str(&session.thread_id) {
            headers.insert("thread-id", value);
        }
    }
    headers
}

/// 官方 installation header（compact/HTTP 形态）。
pub fn installation_header(installation_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(installation_id) {
        headers.insert("x-codex-installation-id", value);
    }
    headers
}

/// 官方 compatibility headers（window/turn-metadata 等）。
pub fn compatibility_headers(projection: &codex_core::CodexResponsesMetadata) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in projection.compatibility_headers() {
        let Some(name) = name else { continue };
        headers.insert(name, value);
    }
    headers
}

/// 执行 Responses HTTP/SSE 原始请求。
pub async fn execute_responses_raw(
    pool: &crate::transport::client_pool::ClientPool,
    installation_id: &str,
    ctx: &RequestContext,
    body: Bytes,
) -> Result<RawUpstreamResponse, LocalError> {
    // 1. 上游 I/O 前校验 JSON/body/marker（§16.2 顺序 6）。
    // round-3 H2：重复 key fail closed（serde_json 默认后者覆盖）。
    metadata::reject_duplicate_keys(std::str::from_utf8(&body).unwrap_or_default())?;
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        LocalError::new(
            "invalid_json",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "responses body is not valid JSON",
        )
    })?;
    metadata::validate_body_object(&value)?;
    let extra = metadata::extract_client_metadata(value.as_object().expect("object"))?;
    if !metadata::big_int_roundtrip_safe(&value) {
        return Err(LocalError::new(
            "unsupported_integer_literal",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "body contains an integer literal outside u64 range",
        ));
    }

    // 2. 官方投影（同一不可变 snapshot；reserved 键 fail closed）。
    let session = ctx.session.as_ref().ok_or_else(|| {
        LocalError::new(
            "missing_session",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "responses requires a session snapshot",
        )
    })?;
    let projection = metadata::build_projection(installation_id, session)?;
    // F12（review）：直接拿所有权，消灭整树深拷贝（value 之后不再使用）。
    let serde_json::Value::Object(mut obj) = value else {
        return Err(LocalError::new(
            "invalid_json",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "responses body must be a JSON object",
        ));
    };
    // sidecar 投影对齐官方内置 OpenAI provider（include_internal_metadata = true）的出站形态。
    // try_for_transport 不设 mcp_attribution，本参数当前不改变投影内容，但必须与官方语义一致。
    metadata::inject_official_client_metadata(
        &mut obj,
        extra,
        projection.client_metadata(/*include_internal*/ true),
    );
    // canonical 位对齐：官方 HTTP 把 turn-metadata 同时放在 body client_metadata
    // 与 compatibility header；projection 由 snapshot 合成拿不到该值，用承载值补。
    metadata::inject_carried_turn_metadata(&mut obj, &ctx.carried_headers);

    // 3. extra headers（空白 HeaderMap 起步；originator/UA 由 client 默认头提供）。
    let mut headers = session_headers(ctx);
    headers.extend(compatibility_headers(&projection));
    // 承载头（FR-004 唯一 header 通道）：Go 侧已按账号身份 scope 完成的
    // 官方 x-codex-* 元数据原样注入并覆盖本地投影，使 rust_sidecar 出站
    // 与 R-Go 直连逐头一致。凭据头（authorization / chatgpt-account-id /
    // cookie）在解析期已 fail closed，不在承载集合内；本地必需头
    // （host/content-length/accept/content-type）与 authorization /
    // chatgpt-account-id 仍由官方 client 与 BearerAuthProvider 自写。
    headers.extend(ctx.carried_header_map());

    // capture 旁路（capture-r1）：请求体校验通过后、发上游前入队
    // （设计 §6.3）。白名单不匹配 = None，零成本原路径。句柄在上游失败
    // 早退时 Drop → best-effort Done(ok=false) → 诚实 .resp.err 终态。
    let capture_handle = crate::capture::begin(ctx);
    if let Some(handle) = &capture_handle {
        handle.send_request(&body);
    }

    // 4. 池化官方 client + 官方 auth。
    let handle = official_adapter::resolve_route_handle(ctx.proxy_kind, ctx.proxy_url.as_ref())?;
    let client = official_adapter::pooled_official_client(
        pool,
        &ctx.account_key,
        &ctx.credential_revision,
        &handle,
        Operation::Responses,
    )
    .await?;
    let auth = official_adapter::official_auth(ctx);
    let provider = official_adapter::chatgpt_provider();
    let api_client =
        codex_api::ResponsesClient::new(official_adapter::transport_for(client), provider, auth);

    // 5. 官方 raw stream（复用 EndpointSession/auth/compression/retry）。
    // R6（R-TRANSPORT-001）：terminal non-2xx 原样保留（不再映射为本地
    // 错误→502），attempt 次数来自官方 retry 循环的真实值。
    let outcome = api_client
        .stream_raw(obj.into(), headers, codex_api::Compression::None, None)
        .await
        .map_err(|err| map_api_error(err, "responses"))?;
    let stream = outcome.value;

    let status = stream.status;
    let headers = crate::contract::filter_upstream_headers(&stream.headers);
    let body_stream = stream
        .bytes
        .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));

    // 响应流旁路 tee（不改流类型；chunk 原样放行，延迟 +0）。
    let body_stream: crate::contract::BodyStream = match capture_handle {
        Some(handle) => Box::pin(crate::capture::CaptureStream::new(body_stream, handle)),
        None => Box::pin(body_stream),
    };

    Ok(RawUpstreamResponse {
        status,
        headers,
        body: Box::pin(body_stream),
        attempt_count: u32::try_from(outcome.attempts).unwrap_or(u32::MAX),
    })
}

/// 将官方 ApiError 映射为本地 typed error（§13 所有权矩阵）。
pub fn map_api_error(err: codex_api::ApiError, operation: &str) -> LocalError {
    let _ = operation;
    match &err {
        codex_api::ApiError::Transport(transport_err) => match transport_err {
            codex_client::TransportError::Timeout => LocalError::new(
                "upstream_timeout",
                ErrorClass::Timeout,
                ErrorPhase::WaitHeaders,
                true,
                "upstream request timed out",
            ),
            codex_client::TransportError::Http { status, .. } => LocalError::new(
                "upstream_http_error",
                ErrorClass::Transport,
                ErrorPhase::WaitHeaders,
                false,
                format!("upstream returned HTTP {status}"),
            ),
            codex_client::TransportError::Network(..) => LocalError::new(
                "upstream_connect_failed",
                ErrorClass::Transport,
                ErrorPhase::Connect,
                true,
                "upstream network failure",
            ),
            codex_client::TransportError::Connection(..) => LocalError::new(
                "upstream_connect_failed",
                ErrorClass::Transport,
                ErrorPhase::Connect,
                true,
                "upstream connection failure",
            ),
            codex_client::TransportError::Build(..) => LocalError::new(
                "upstream_build_error",
                ErrorClass::Contract,
                ErrorPhase::WriteRequest,
                false,
                "upstream request build failure",
            ),
            codex_client::TransportError::RetryLimit => LocalError::new(
                "retry_limit_reached",
                ErrorClass::Transport,
                ErrorPhase::Connect,
                true,
                "official retry budget exhausted",
            ),
            // 既有变体：官方 transport 的有界 body 保护触发时，
            // 上游已经发出超过本地配置上限的响应体。与 compact/auxiliary 的
            // unary 上限同码同阶段：fail closed，不重试。
            codex_client::TransportError::ResponseTooLarge { max_bytes } => LocalError::new(
                "upstream_body_too_large",
                ErrorClass::UpstreamProtocol,
                ErrorPhase::StreamBody,
                false,
                format!("upstream response body exceeds {max_bytes} bytes"),
            ),
            // 0.159.0 新增变体：codex-http-client 的应用层网络策略拒绝。
            // NetworkPolicyDenied 是确定性拒绝（官方 RetryOn 判定不可重试、
            // api_bridge 映射为 Fatal），本地按 config 类 fail closed：
            // 不重试、绝不回退 direct。
            codex_client::TransportError::Policy(denied) => LocalError::new(
                "network_policy_denied",
                ErrorClass::Config,
                ErrorPhase::Connect,
                false,
                format!("destination denied by application network policy: {denied}"),
            ),
        },
        codex_api::ApiError::Stream(..) => LocalError::new(
            "upstream_stream_error",
            ErrorClass::UpstreamProtocol,
            ErrorPhase::StreamBody,
            false,
            "upstream stream error",
        ),
        _ => LocalError::new(
            "upstream_error",
            ErrorClass::Transport,
            ErrorPhase::WaitHeaders,
            false,
            "upstream error",
        ),
    }
}

#[allow(dead_code)]
fn _method_probe() -> Method {
    Method::POST
}

#[cfg(test)]
mod tests {
    use super::session_headers;
    use codex_core::CodexResponsesMetadata;

    #[test]
    fn projection_roundtrips_official_metadata() {
        let metadata = CodexResponsesMetadata::try_for_transport(
            "installation-1".to_string(),
            "0198f2b0-0000-7000-8000-000000000001".to_string(),
            "0198f2b0-0000-7000-8000-000000000002".to_string(),
            "0198f2b0-0000-7000-8000-000000000002:1".to_string(),
        )
        .expect("valid projection");
        // 与生产投影一致：对齐官方内置 provider 的 include_internal_metadata = true。
        let client_metadata = metadata.client_metadata(/*include_internal*/ true);
        assert_eq!(
            client_metadata.get("session_id").unwrap(),
            "0198f2b0-0000-7000-8000-000000000001"
        );
        assert_eq!(
            client_metadata.get("x-codex-window-id").unwrap(),
            "0198f2b0-0000-7000-8000-000000000002:1"
        );
    }

    #[test]
    fn upstream_headers_never_contain_internal_namespace() {
        // T3 负向（红线）：x-ctd-user-id 等内部 header 绝不进入上游投影。
        // session_headers 由空白 HeaderMap 构造，仅官方 session-id/thread-id。
        let ctx = crate::contract::RequestContext {
            session: Some(crate::contract::headers::SessionSnapshot {
                session_id: "0198f2b0-0000-7000-8000-000000000001".to_string(),
                thread_id: "0198f2b0-0000-7000-8000-000000000002".to_string(),
                window_id: "0198f2b0-0000-7000-8000-000000000002:1".to_string(),
            }),
            user_id: Some("3".to_string()),
            ..Default::default()
        };
        let upstream = session_headers(&ctx);
        assert!(upstream.get("x-ctd-user-id").is_none());
        for (name, _) in upstream.iter() {
            assert!(
                !name.as_str().starts_with("x-ctd-"),
                "internal header {name} leaked to upstream projection"
            );
        }
    }

    #[test]
    fn invalid_session_fails_before_io() {
        let err = CodexResponsesMetadata::try_for_transport(
            "i".to_string(),
            "not-a-uuid".to_string(),
            "0198f2b0-0000-7000-8000-000000000002".to_string(),
            "0198f2b0-0000-7000-8000-000000000002:1".to_string(),
        )
        .expect_err("invalid session");
        assert!(err.to_string().contains("UUIDv7"));
    }
}
