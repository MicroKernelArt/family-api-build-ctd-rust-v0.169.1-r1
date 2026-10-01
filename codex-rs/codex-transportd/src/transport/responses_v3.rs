//! v3（`X-CTD-Hookpoint: v3`）Responses 族**原样透传**适配（docs/20 §3.5 / §4.3 M5+M6）。
//!
//! v3 与 v1 是同一 ctd 二进制内的两套实现，共享同一 operation 集、同一 UDS
//! 协议、同一响应形态（[`RawUpstreamResponse`]）与同一错误语义；差异**只在
//! "请求如何被构造"这一层**：
//!
//! | | v1 | v3 |
//! |---|---|---|
//! | headers | Go 交付白名单**子集**，ctd 自派生其余 | Go 交付白名单**全集**，ctd 只做白名单过滤 |
//! | body | ctd 解析 → 改投影 → `serde_json` 重编码 | Go 的**最终字节**，ctd 不解析不改写 |
//! | `content-encoding` | ctd 权威，恒 identity | **Go 权威**，ctd 只校验 |
//!
//! ★ 边界处理（认证注入 / 超时 / 重试 / SSE 转发 / 审计）全部保留，且由
//! 与 v1 **同一段**代码承担：认证注入在
//! `codex-api/src/endpoint/session.rs` 的 `auth.apply_auth` 闭包内，重试走
//! 同一官方 `run_with_request_telemetry_preserving_status`，SSE 仍是逐块
//! 转发（不 parse / 不 re-serialize）。
//!
//! ★★ body 形态 ↔ `content-encoding` 一致性规则（docs/20 §3.3）
//!
//! | # | 规则 |
//! |---|---|
//! | R1 | `content-encoding` 与 body 编码由**同一权威**产生，ctd 结构上不可能制造不一致 |
//! | R3 | v3：Go 是权威；ctd 用 `RequestBody::Raw(bytes)` + `RequestCompression::None` 原样发送，**既不写也不改** `content-encoding` / `content-type`（`codex-http-client/src/request.rs` 的 `Raw` 分支只 clone）。`Raw` + 压缩会直接报错 ⇒ 天然 fail closed |
//! | R4 | ctd 做**形状校验**（审计，非决策）：`content-encoding` 若存在必须 ∈ [`V3_CONTENT_ENCODINGS`]，否则 `invalid_content_encoding` |
//! | R5 | `host` / `content-length` / `accept-encoding` 是 ctd-owned 传输头：不在 [`V3_HEADER_ALLOWLIST`] ⇒ 即使 Go 误传也不进入出站集 |
//! | R6 | `content-type` 必须由 Go 提供，ctd 不补；缺失 ⇒ `missing_v3_required_header` |
//! | R7 | 灰度顺序先 identity 后 zstd；契约当天即允许 zstd（对齐官方只需改 Go 一处，ctd 零改动） |
//!
//! 本模块另外实现一条**比 R4 更强**的一致性检查（[`check_v3_body_encoding`]）：
//! 声明 `zstd` ⇒ body 必须带 zstd frame magic；未声明 ⇒ body 不得带该 magic。
//! 这是"上游解压失败"的唯一结构性防线——单靠闭集校验挡不住
//! "Go 压了但忘写头"这一方向。

use bytes::Bytes;
use futures::StreamExt;
use http::HeaderMap;

use crate::contract::RawUpstreamResponse;
use crate::contract::RequestContext;
use crate::contract::error::ErrorClass;
use crate::contract::error::ErrorPhase;
use crate::contract::error::LocalError;
use crate::contract::headers::V3_AUDIT_HEADERS;
use crate::contract::headers::V3_CONTENT_ENCODINGS;
use crate::contract::headers::V3_REQUIRED_HEADERS;
use crate::contract::operation::Operation;
use crate::transport::official_adapter;
use crate::transport::responses::map_api_error;

/// zstd frame magic（小端 `0xFD2FB528`）。
pub const ZSTD_FRAME_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// v3 body 形态：由 Go 交付的 `content-encoding` 决定（ctd 只读不写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3BodyEncoding {
    /// 明文（无 `content-encoding`）——首期灰度形态（R7）。
    Identity,
    /// zstd 压缩体（`content-encoding: zstd`）——对齐官方 159.2 的形态。
    Zstd,
}

impl V3BodyEncoding {
    /// 闭集解析；未知值返回 `None`（调用方 fail closed）。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "zstd" => Some(Self::Zstd),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Zstd => "zstd",
        }
    }
}

fn invalid_content_encoding(raw: &str) -> LocalError {
    LocalError::new(
        "invalid_content_encoding",
        ErrorClass::Contract,
        ErrorPhase::Validate,
        false,
        format!(
            "v3 的 content-encoding 只允许 {:?}（或缺失 = identity），收到 {raw:?}",
            V3_CONTENT_ENCODINGS
        ),
    )
}

fn audit_failed(message: impl Into<String>) -> LocalError {
    LocalError::new(
        "hookpoint_audit_failed",
        ErrorClass::Contract,
        ErrorPhase::Validate,
        false,
        message,
    )
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

/// v3 必需头校验（R6）：`accept` / `content-type` 缺失 ⇒ 发出前 fail closed。
///
/// 理由：v3 下 ctd **不做 header 决策**（不补 `content-type`、不写 `accept`），
/// 缺失即上游 4xx；必须在任何 upstream I/O 之前拦住。
pub fn check_v3_required_headers(headers: &HeaderMap) -> Result<(), LocalError> {
    for name in V3_REQUIRED_HEADERS {
        let present = headers
            .get(*name)
            .map(|value| !value.is_empty())
            .unwrap_or(false);
        if !present {
            return Err(LocalError::new(
                "missing_v3_required_header",
                ErrorClass::Contract,
                ErrorPhase::Validate,
                false,
                format!("v3 要求 Go 交付完整出站头集：缺少必需头 {name}（ctd 不代补）"),
            ));
        }
    }
    Ok(())
}

/// 解析 Go 声明的 body 形态（R4 闭集校验，缺省 identity）。
pub fn resolve_v3_body_encoding(headers: &HeaderMap) -> Result<V3BodyEncoding, LocalError> {
    let Some(value) = headers.get("content-encoding") else {
        return Ok(V3BodyEncoding::Identity);
    };
    let raw = value
        .to_str()
        .map_err(|_| invalid_content_encoding("<non-ascii>"))?;
    V3BodyEncoding::parse(raw).ok_or_else(|| invalid_content_encoding(raw))
}

/// ★ body 形态 ↔ `content-encoding` 一致性（R1 的可执行形态）。
///
/// - 声明 `zstd` ⇒ body 必须以 zstd frame magic 开头；
/// - 未声明（identity）⇒ body 不得以该 magic 开头。
///
/// 两个方向都 fail closed：前者挡住"写了头没压"，后者挡住"压了没写头"
/// ——后者正是上游解压失败的直接成因。
pub fn check_v3_body_encoding(
    encoding: V3BodyEncoding,
    body: &[u8],
) -> Result<(), LocalError> {
    let has_zstd_frame = body.len() >= ZSTD_FRAME_MAGIC.len() && body[..4] == ZSTD_FRAME_MAGIC;
    match (encoding, has_zstd_frame) {
        (V3BodyEncoding::Zstd, true) | (V3BodyEncoding::Identity, false) => Ok(()),
        (V3BodyEncoding::Zstd, false) => Err(LocalError::new(
            "content_encoding_body_mismatch",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "content-encoding=zstd 但 body 不含 zstd frame magic：形态与声明不一致",
        )),
        (V3BodyEncoding::Identity, true) => Err(LocalError::new(
            "content_encoding_body_mismatch",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "body 带 zstd frame magic 但未声明 content-encoding：上游会按明文解压失败",
        )),
    }
}

/// v3 审计（docs/20 §3.5 项 1–4，+ docs/21 I2 的 thread-id 自洽）。
///
/// v3 不改变任何不变式的权威归属（全部仍在 Go 侧）——它只是去掉 ctd 的
/// "二次取值"，把 ctd 从"共同权威"降为"纯消费者 + 审计者"。因此这些检查
/// 是**确定性的廉价头内自洽检查**，失败即 fail closed（`hookpoint_audit_failed`）。
///
/// | # | 不变式 |
/// |---|---|
/// | 1 | `session-id` == `x-codex-turn-metadata.session_id` |
/// | 2 | `thread-id` == `x-codex-turn-metadata.thread_id` |
/// | 3 | `x-codex-window-id` == `x-codex-turn-metadata.window_id` |
/// | 4 | `x-client-request-id` == `session-id` |
/// | 5 | `x-codex-window-id` 后缀 == `x-codex-turn-metadata.window_number` |
///
/// 第 5 条（首轮 `turn_id == root_turn_id`）**不做**：ctd 无法判定"是否首轮"，
/// 归 Go 侧责任（docs/20 §3.5）。
///
/// ★ 存在性要求（`V3_AUDIT_HEADERS` 全部必须存在）：这是对 **Go 交付契约**的
/// 要求，不是对客户端的要求。v1 下 ctd 会从 session snapshot **合成**
/// `x-codex-turn-metadata`（`compatibility_headers(&projection)`），v3 下 ctd
/// 不合成 ⇒ 若某条 Go 出站路径没有交付这些头，v3 会在此 fail closed，而不是
/// 把一个 ctd 无法背书的请求发出去。已知边界：raw passthrough 路径在**客户端
/// 未发送** `x-codex-turn-metadata` 时不会合成该头
/// （`rewriteCodexTurnMetadataFields` 对缺失头直接返回），此时 v3 会拒绝——
/// 该请求类在 R-Go 直连下同样缺该头（只是少了元数据，不报错）。灰度时若命中，
/// 应按"Go 交付不完整"处理（补 Go 侧合成），不得放宽本检查。
pub fn audit_v3_headers(headers: &HeaderMap) -> Result<(), LocalError> {
    for name in V3_AUDIT_HEADERS {
        if !headers.contains_key(*name) {
            return Err(audit_failed(format!(
                "v3 审计失败：缺少审计头 {name}（Go 交付的完整出站头集不完整）"
            )));
        }
    }

    let session_id = header_str(headers, "session-id");
    let thread_id = header_str(headers, "thread-id");
    let window_id = header_str(headers, "x-codex-window-id");
    let client_request_id = header_str(headers, "x-client-request-id");

    let raw = header_str(headers, "x-codex-turn-metadata");
    let parsed: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| audit_failed("v3 审计失败：x-codex-turn-metadata 不是合法 JSON"))?;
    let turn_metadata = parsed
        .as_object()
        .ok_or_else(|| audit_failed("v3 审计失败：x-codex-turn-metadata 不是 JSON object"))?;

    let field = |name: &str| turn_metadata.get(name).and_then(|value| value.as_str());

    if field("session_id") != Some(session_id) {
        return Err(audit_failed(
            "v3 审计失败：session-id 与 x-codex-turn-metadata.session_id 不同源",
        ));
    }
    if field("thread_id") != Some(thread_id) {
        return Err(audit_failed(
            "v3 审计失败：thread-id 与 x-codex-turn-metadata.thread_id 不同源",
        ));
    }
    if field("window_id") != Some(window_id) {
        return Err(audit_failed(
            "v3 审计失败：x-codex-window-id 与 x-codex-turn-metadata.window_id 不同源",
        ));
    }
    if client_request_id != session_id {
        return Err(audit_failed(
            "v3 审计失败：x-client-request-id 与 session-id 不同值（官方不变式）",
        ));
    }
    let window_number = turn_metadata
        .get("window_number")
        .and_then(|value| value.as_u64());
    let window_suffix = window_id
        .rsplit_once(':')
        .and_then(|(_, suffix)| suffix.parse::<u64>().ok());
    if window_number.is_none() || window_number != window_suffix {
        return Err(audit_failed(
            "v3 审计失败：x-codex-window-id 后缀与 x-codex-turn-metadata.window_number 不一致",
        ));
    }
    Ok(())
}

/// v3 出站头集准备：白名单过滤 → 必需头 → body 形态一致性 → 审计。
///
/// 全部在任何 upstream I/O 之前完成；任一步失败即 fail closed（绝不静默
/// 回落 v1 —— 回落会污染 A/B 观测，且与既有"rust_sidecar 无自动回退"
/// 纪律冲突，docs/20 §2.5）。
pub fn prepare_v3_headers(ctx: &RequestContext, body: &[u8]) -> Result<HeaderMap, LocalError> {
    let headers = ctx.outbound_header_map();
    check_v3_required_headers(&headers)?;
    let encoding = resolve_v3_body_encoding(&headers)?;
    check_v3_body_encoding(encoding, body)?;
    audit_v3_headers(&headers)?;
    Ok(headers)
}

/// 执行 v3 Responses HTTP/SSE 原样透传。
pub async fn execute_responses_v3_raw(
    pool: &crate::transport::client_pool::ClientPool,
    ctx: &RequestContext,
    body: Bytes,
) -> Result<RawUpstreamResponse, LocalError> {
    let headers = prepare_v3_headers(ctx, &body)?;

    // capture 旁路：与 v1 同一入队点（校验通过后、发上游前）。
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
        Operation::Responses,
    )
    .await?;
    let auth = official_adapter::official_auth(ctx);
    // ★ bare provider：v3 的头集完全由 Go 决定，provider 不得再贡献 version。
    let provider = official_adapter::chatgpt_provider_bare();
    let api_client =
        codex_api::ResponsesClient::new(official_adapter::transport_for(client), provider, auth);

    let outcome = api_client
        .stream_raw_passthrough(body, headers)
        .await
        .map_err(|err| map_api_error(err, "responses"))?;
    let stream = outcome.value;

    let status = stream.status;
    let headers = crate::contract::filter_upstream_headers(&stream.headers);
    let body_stream = stream
        .bytes
        .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));

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

/// 执行 v3 compact 原样透传（与 responses 同规则、同校验，只是 unary）。
pub async fn execute_compact_v3_raw(
    pool: &crate::transport::client_pool::ClientPool,
    ctx: &RequestContext,
    body: Bytes,
) -> Result<RawUpstreamResponse, LocalError> {
    let headers = prepare_v3_headers(ctx, &body)?;

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
    let provider = official_adapter::chatgpt_provider_bare();
    let api_client =
        codex_api::ResponsesClient::new(official_adapter::transport_for(client), provider, auth);

    let outcome = api_client
        .execute_raw_passthrough(
            body,
            headers,
            crate::transport::compact::COMPACT_REQUEST_TIMEOUT,
        )
        .await
        .map_err(|err| map_api_error(err, "compact"))?;
    let response = outcome.value;

    let status = response.status;
    let headers = crate::contract::filter_upstream_headers(&response.headers);
    let bytes = response.body;
    // F10（review）：compact unary 响应体同样有界（与 v1 compact 一致）。
    const MAX_UNARY_BODY: usize = 128 * 1024 * 1024;
    if bytes.len() > MAX_UNARY_BODY {
        return Err(LocalError::new(
            "upstream_body_too_large",
            ErrorClass::UpstreamProtocol,
            ErrorPhase::StreamBody,
            false,
            format!(
                "compact response body {} bytes exceeds unary limit {MAX_UNARY_BODY}",
                bytes.len()
            ),
        ));
    }

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
    use crate::contract::headers::V3_HEADER_ALLOWLIST;

    fn v3_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                http::header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                http::HeaderValue::from_str(value).expect("value"),
            );
        }
        headers
    }

    /// 一份头内自洽的 v3 头集（官方 159.2 exec 形态，值已归一化）。
    fn consistent_headers() -> HeaderMap {
        let turn_metadata = r#"{"session_id":"0198f2b0-0000-7000-8000-000000000001","thread_id":"0198f2b0-0000-7000-8000-000000000002","window_id":"0198f2b0-0000-7000-8000-000000000002:1","window_number":1}"#;
        v3_headers(&[
            ("accept", "text/event-stream"),
            ("content-type", "application/json"),
            ("version", "0.159.2"),
            ("session-id", "0198f2b0-0000-7000-8000-000000000001"),
            ("thread-id", "0198f2b0-0000-7000-8000-000000000002"),
            (
                "x-client-request-id",
                "0198f2b0-0000-7000-8000-000000000001",
            ),
            (
                "x-codex-window-id",
                "0198f2b0-0000-7000-8000-000000000002:1",
            ),
            ("x-codex-turn-metadata", turn_metadata),
        ])
    }

    #[test]
    fn required_headers_fail_closed_when_missing() {
        let mut headers = consistent_headers();
        headers.remove("content-type");
        let err = check_v3_required_headers(&headers).expect_err("content-type 必需");
        assert_eq!(err.code, "missing_v3_required_header");
        assert_eq!(err.class, ErrorClass::Contract);
        assert_eq!(err.phase, ErrorPhase::Validate);

        let mut headers = consistent_headers();
        headers.remove("accept");
        let err = check_v3_required_headers(&headers).expect_err("accept 必需");
        assert_eq!(err.code, "missing_v3_required_header");
    }

    #[test]
    fn required_headers_pass_when_present() {
        check_v3_required_headers(&consistent_headers()).expect("完整头集通过");
    }

    #[test]
    fn content_encoding_closed_set() {
        assert_eq!(
            resolve_v3_body_encoding(&consistent_headers()).expect("identity"),
            V3BodyEncoding::Identity
        );
        let headers = v3_headers(&[("content-encoding", "zstd")]);
        assert_eq!(
            resolve_v3_body_encoding(&headers).expect("zstd"),
            V3BodyEncoding::Zstd
        );
        // 闭集之外一律 fail closed（ctd/上游链路未验证的编码不得透传）。
        for bad in ["gzip", "br", "deflate", "identity", "zstd, gzip"] {
            let headers = v3_headers(&[("content-encoding", bad)]);
            let err = resolve_v3_body_encoding(&headers).expect_err(bad);
            assert_eq!(err.code, "invalid_content_encoding", "{bad}");
        }
    }

    #[test]
    fn content_encoding_closed_set_matches_enum() {
        // 闭集常量与枚举必须同步（新增编码时两处一起改）。
        assert_eq!(V3_CONTENT_ENCODINGS, &["zstd"]);
        for name in V3_CONTENT_ENCODINGS {
            assert!(
                V3BodyEncoding::parse(name).is_some(),
                "闭集值 {name} 必须能被 V3BodyEncoding::parse 解析"
            );
        }
        assert_eq!(V3BodyEncoding::Zstd.as_str(), "zstd");
        assert_eq!(V3BodyEncoding::Identity.as_str(), "identity");
    }

    #[test]
    fn body_encoding_consistency_both_directions() {
        let plain = br#"{"model":"gpt-5"}"#;
        let compressed = [ZSTD_FRAME_MAGIC.as_slice(), b"payload"].concat();

        // 一致的两组通过。
        check_v3_body_encoding(V3BodyEncoding::Identity, plain).expect("identity+明文");
        check_v3_body_encoding(V3BodyEncoding::Zstd, &compressed).expect("zstd+压缩体");

        // ★ "写了头没压"：上游解压失败。
        let err = check_v3_body_encoding(V3BodyEncoding::Zstd, plain).expect_err("必须 fail closed");
        assert_eq!(err.code, "content_encoding_body_mismatch");
        assert_eq!(err.class, ErrorClass::Contract);
        assert_eq!(err.phase, ErrorPhase::Validate);

        // ★ "压了没写头"：上游按明文解压失败。
        let err =
            check_v3_body_encoding(V3BodyEncoding::Identity, &compressed).expect_err("必须 fail closed");
        assert_eq!(err.code, "content_encoding_body_mismatch");

        // 短 body 不越界读。
        check_v3_body_encoding(V3BodyEncoding::Identity, &[0x28, 0xb5]).expect("短 body");
        check_v3_body_encoding(V3BodyEncoding::Zstd, &[]).expect_err("空 body 不可能是 zstd frame");
    }

    #[test]
    fn audit_accepts_consistent_headers() {
        audit_v3_headers(&consistent_headers()).expect("自洽头集通过");
    }

    #[test]
    fn audit_rejects_missing_audit_header() {
        for name in V3_AUDIT_HEADERS {
            let mut headers = consistent_headers();
            headers.remove(*name);
            let err = audit_v3_headers(&headers).expect_err(name);
            assert_eq!(err.code, "hookpoint_audit_failed", "{name}");
        }
    }

    #[test]
    fn audit_rejects_header_body_disagreement() {
        // I1：session-id 与 tm.session_id 不同源。
        let mut headers = consistent_headers();
        headers.insert(
            "session-id",
            http::HeaderValue::from_static("0198f2b0-0000-7000-8000-00000000dead"),
        );
        assert_eq!(
            audit_v3_headers(&headers).expect_err("I1").code,
            "hookpoint_audit_failed"
        );

        // I2：thread-id 不同源。
        let mut headers = consistent_headers();
        headers.insert(
            "thread-id",
            http::HeaderValue::from_static("0198f2b0-0000-7000-8000-00000000dead"),
        );
        assert_eq!(
            audit_v3_headers(&headers).expect_err("I2").code,
            "hookpoint_audit_failed"
        );

        // I3：x-codex-window-id 不同源。
        let mut headers = consistent_headers();
        headers.insert(
            "x-codex-window-id",
            http::HeaderValue::from_static("0198f2b0-0000-7000-8000-000000000002:2"),
        );
        assert_eq!(
            audit_v3_headers(&headers).expect_err("I3").code,
            "hookpoint_audit_failed"
        );

        // I4：x-client-request-id != session-id。
        let mut headers = consistent_headers();
        headers.insert(
            "x-client-request-id",
            http::HeaderValue::from_static("0198f2b0-0000-7000-8000-00000000dead"),
        );
        assert_eq!(
            audit_v3_headers(&headers).expect_err("I4").code,
            "hookpoint_audit_failed"
        );

        // I5：window 后缀与 window_number 不一致。
        let mut headers = consistent_headers();
        headers.insert(
            "x-codex-turn-metadata",
            http::HeaderValue::from_static(
                r#"{"session_id":"0198f2b0-0000-7000-8000-000000000001","thread_id":"0198f2b0-0000-7000-8000-000000000002","window_id":"0198f2b0-0000-7000-8000-000000000002:1","window_number":7}"#,
            ),
        );
        assert_eq!(
            audit_v3_headers(&headers).expect_err("I5").code,
            "hookpoint_audit_failed"
        );

        // turn-metadata 不是合法 JSON。
        let mut headers = consistent_headers();
        headers.insert(
            "x-codex-turn-metadata",
            http::HeaderValue::from_static("not-json"),
        );
        assert_eq!(
            audit_v3_headers(&headers).expect_err("json").code,
            "hookpoint_audit_failed"
        );
    }

    #[test]
    fn allowlist_excludes_transport_owned_and_credential_headers() {
        // R5：ctd-owned 传输头与凭据头在结构上不可能进入 v3 出站集。
        for name in [
            "host",
            "content-length",
            "accept-encoding",
            "authorization",
            "chatgpt-account-id",
            "cookie",
        ] {
            assert!(
                !V3_HEADER_ALLOWLIST.contains(&name),
                "{name} 不得进入 v3 白名单"
            );
        }
    }
}
