//! Compact 原始适配（TLS-CTD-001 §12.2 / FR-012）。
//!
//! Rust 只执行官方 raw compact（`ResponsesClient::execute_raw_compact`）并原样返回
//! upstream status/headers/body；window advance 由 Go 在完整成功判定后
//! 执行（Rust 绝不推进 Go window）。

use std::time::Duration;

use bytes::Bytes;

use crate::contract::error::ErrorClass;
use crate::contract::error::ErrorPhase;
use crate::contract::error::LocalError;
use crate::contract::operation::Operation;
use crate::contract::RawUpstreamResponse;
use crate::contract::RequestContext;
use crate::transport::metadata;
use crate::transport::official_adapter;
use crate::transport::responses;

/// Compact 请求超时（官方 300s idle × 4）。
const COMPACT_REQUEST_TIMEOUT: Duration = Duration::from_secs(1200);

/// 执行官方 raw compact。
pub async fn execute_compact_raw(
    pool: &crate::transport::client_pool::ClientPool,
    installation_id: &str,
    ctx: &RequestContext,
    body: Bytes,
) -> Result<RawUpstreamResponse, LocalError> {
    // round-3 H2：重复 key fail closed。
    metadata::reject_duplicate_keys(std::str::from_utf8(&body).unwrap_or_default())?;
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        LocalError::new(
            "invalid_json",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "compact body is not valid JSON",
        )
    })?;
    metadata::validate_body_object(&value)?;
    if !metadata::big_int_roundtrip_safe(&value) {
        return Err(LocalError::new(
            "unsupported_integer_literal",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "body contains an integer literal outside u64 range",
        ));
    }

    let session = ctx.session.as_ref().ok_or_else(|| {
        LocalError::new(
            "missing_session",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "compact requires a session snapshot",
        )
    })?;
    let projection = metadata::build_projection(installation_id, session)?;

    // 官方 compact 形态：installation header + session headers +
    // compatibility headers（不注入 normal Responses body metadata，§11.5）。
    let mut headers = responses::installation_header(installation_id);
    headers.extend(responses::session_headers(ctx));
    headers.extend(responses::compatibility_headers(&projection));

    // capture 旁路（capture-r1）：请求体校验通过后、发上游前入队；
    // 上游失败早退时句柄 Drop → 诚实 .resp.err 终态。
    let capture_handle = crate::capture::begin(ctx);
    if let Some(handle) = &capture_handle {
        handle.send_request(&body);
    }

    let handle = official_adapter::resolve_route_handle(ctx.proxy_kind, ctx.proxy_url.as_ref())?;
    let client = official_adapter::pooled_official_client(
        pool,
        &ctx.account_key,
        &ctx.credential_revision,
        &handle,
        Operation::ResponsesCompact,
    )
    .await?;
    let auth = official_adapter::official_auth(ctx);
    let provider = official_adapter::chatgpt_provider();
    let api_client =
        codex_api::ResponsesClient::new(official_adapter::transport_for(client), provider, auth);

    // R6（R-TRANSPORT-001）：terminal non-2xx 原样保留（不映射为本地
    // 错误→502），attempt 次数来自官方 retry 循环的真实值。
    let outcome = api_client
        .execute_raw_compact(value, headers, COMPACT_REQUEST_TIMEOUT)
        .await
        .map_err(|err| responses::map_api_error(err, "compact"))?;
    let response = outcome.value;

    let status = response.status;
    let headers = crate::contract::filter_upstream_headers(&response.headers);
    let bytes = response.body;
    // F10（review）：compact unary 响应体同样有界（与 auxiliary 一致）。
    const MAX_UNARY_BODY: usize = 128 * 1024 * 1024;
    if bytes.len() > MAX_UNARY_BODY {
        return Err(crate::contract::error::LocalError::new(
            "upstream_body_too_large",
            crate::contract::error::ErrorClass::UpstreamProtocol,
            crate::contract::error::ErrorPhase::StreamBody,
            false,
            format!(
                "compact response body {} bytes exceeds unary limit {MAX_UNARY_BODY}",
                bytes.len()
            ),
        ));
    }

    // 响应旁路 tee（compact 同 responses，共 2 处 hook）。
    let body_stream: crate::contract::BodyStream = match capture_handle {
        Some(handle) => Box::pin(crate::capture::CaptureStream::new(
            futures::stream::once(Box::pin(async move { Ok::<_, std::io::Error>(bytes) })),
            handle,
        )),
        None => Box::pin(futures::stream::once(Box::pin(async move {
            Ok::<_, std::io::Error>(bytes)
        }))),
    };

    Ok(RawUpstreamResponse {
        status,
        headers,
        body: Box::pin(body_stream),
        attempt_count: u32::try_from(outcome.attempts).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_timeout_is_bounded() {
        assert_eq!(COMPACT_REQUEST_TIMEOUT, Duration::from_secs(1200));
    }
}
