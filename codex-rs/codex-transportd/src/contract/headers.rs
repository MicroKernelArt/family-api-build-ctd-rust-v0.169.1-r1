//! 内部请求 header 契约（TLS-CTD-001 §11.4 / §16.2）。
//!
//! 所有 `X-CTD-*` header 在入口解析后即被消费并丢弃；Rust 从空白
//! `HeaderMap` 经官方 builder 构造 upstream 请求，禁止
//! `headers.extend(internal_request.headers())`。

use std::time::Instant;

use http::HeaderMap;
use http::HeaderValue;

use super::error::ErrorClass;
use super::error::ErrorPhase;
use super::error::LocalError;
use crate::contract::operation::Operation;
use crate::redaction::SecretString;

pub const PROTOCOL_VERSION: &str = "1";
pub const PROTOCOL_VERSION_HEADER: &str = "x-ctd-protocol-version";
pub const REQUEST_ID_HEADER: &str = "x-ctd-request-id";
pub const ACCOUNT_KEY_HEADER: &str = "x-ctd-account-key";
pub const CREDENTIAL_REVISION_HEADER: &str = "x-ctd-credential-revision";
pub const ACCESS_TOKEN_HEADER: &str = "x-ctd-access-token";
pub const CHATGPT_ACCOUNT_ID_HEADER: &str = "x-ctd-chatgpt-account-id";
pub const PROXY_KIND_HEADER: &str = "x-ctd-proxy-kind";
pub const PROXY_URL_HEADER: &str = "x-ctd-proxy-url";
pub const SESSION_ID_HEADER: &str = "x-ctd-session-id";
pub const THREAD_ID_HEADER: &str = "x-ctd-thread-id";
pub const WINDOW_ID_HEADER: &str = "x-ctd-window-id";
pub const PURPOSE_HEADER: &str = "x-ctd-purpose";
pub const DEADLINE_UNIX_MS_HEADER: &str = "x-ctd-deadline-unix-ms";
/// admission 等待上限（毫秒；M4：Go cfg.AdmissionTimeoutMS 传入）。
pub const ADMISSION_TIMEOUT_MS_HEADER: &str = "x-ctd-admission-timeout-ms";
pub const IF_NONE_MATCH_HEADER: &str = "x-ctd-if-none-match";
/// OAuth refresh token(P1 auth 平面;仅 UDS,敏感,绝不记录)。
pub const REFRESH_TOKEN_HEADER: &str = "x-ctd-refresh-token";
/// Agent identity 私钥(P1;仅 UDS,敏感,绝不记录)。
pub const AGENT_PRIVATE_KEY_HEADER: &str = "x-ctd-agent-private-key";
/// Agent identity runtime id。
pub const AGENT_RUNTIME_ID_HEADER: &str = "x-ctd-agent-runtime-id";
/// OAuth client_id(auth 平面;Sub2API 自有 OAuth app,非官方常量)。
pub const OAUTH_CLIENT_ID_HEADER: &str = "x-ctd-oauth-client-id";
pub const FILE_ID_HEADER: &str = "x-ctd-file-id";
pub const CONVERSATION_ID_HEADER: &str = "x-ctd-conversation-id";
pub const ATTACHMENT_ID_HEADER: &str = "x-ctd-attachment-id";
/// capture 白名单身份（capture-r1；仅 UDS，Go 从认证后的 API Key 注入；
/// 只允许 capture 模块消费，禁止加入任何上游投影）。
pub const USER_ID_HEADER: &str = "x-ctd-user-id";

/// 请求目的（仅低基数观测，不成为 upstream header/body 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestPurpose {
    #[default]
    Request,
    Probe,
    AccountTest,
}

impl RequestPurpose {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("probe") => Self::Probe,
            Some("account_test") => Self::AccountTest,
            _ => Self::Request,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Probe => "probe",
            Self::AccountTest => "account_test",
        }
    }
}

/// 解析后的不可变请求上下文（secret 已封装）。
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub request_id: String,
    pub account_key: String,
    pub credential_revision: String,
    pub access_token: SecretString,
    pub chatgpt_account_id: Option<SecretString>,
    pub operation: Operation,
    pub proxy_kind: ProxyKind,
    pub proxy_url: Option<SecretString>,
    pub session: Option<SessionSnapshot>,
    pub purpose: RequestPurpose,
    pub deadline: Option<Instant>,
    /// admission 等待上限（毫秒；M4，缺失用常量）。
    pub admission_timeout_ms: Option<u64>,
    pub if_none_match: Option<String>,
    /// OAuth refresh token(仅 auth operation 使用)。
    pub refresh_token: Option<SecretString>,
    /// Agent identity 私钥(PKCS8 base64,仅 agent operation 使用)。
    pub agent_private_key: Option<SecretString>,
    pub agent_runtime_id: Option<String>,
    pub oauth_client_id: Option<String>,
    pub file_id: Option<String>,
    pub conversation_id: Option<String>,
    pub attachment_id: Option<String>,
    /// capture 白名单身份（capture-r1；缺省 None，不 fail；仅 capture 模块消费）。
    pub user_id: Option<String>,
    /// 内部 traceparent（只建立 Go→Rust child span；不得复制到 upstream）。
    pub traceparent: Option<String>,
}

/// 账号级 proxy route（TLS-CTD-001 §14）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyKind {
    #[default]
    Direct,
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl ProxyKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            "socks5" => Some(Self::Socks5),
            "socks5h" => Some(Self::Socks5h),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
            Self::Socks5h => "socks5h",
        }
    }
}

/// 经验证的 v0_144 session 不可变快照（TLS-CTD-001 §11.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub thread_id: String,
    pub window_id: String,
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value: &HeaderValue| value.to_str().ok())
}

fn required(headers: &HeaderMap, name: &str, err: &mut Option<LocalError>) -> Option<String> {
    match header_str(headers, name) {
        Some(value) if !value.is_empty() => Some(value.to_string()),
        _ => {
            if err.is_none() {
                *err = Some(super::error::codes::missing_required_header(name));
            }
            None
        }
    }
}

/// 校验协议版本与必需 header，并构造不可变请求上下文。
///
/// 该函数在 admission 阶段（任何 DNS/proxy/upstream I/O 之前）完成全部
/// 字段校验（TLS-CTD-001 §16.2 顺序 2–6）。
pub fn parse_request_context(
    headers: &HeaderMap,
    operation: Operation,
) -> Result<RequestContext, LocalError> {
    // 2. protocol version
    let version = header_str(headers, PROTOCOL_VERSION_HEADER).unwrap_or("");
    if version != PROTOCOL_VERSION {
        return Err(super::error::codes::invalid_protocol_version());
    }

    let mut err: Option<LocalError> = None;
    let request_id = required(headers, REQUEST_ID_HEADER, &mut err);
    let account_key = required(headers, ACCOUNT_KEY_HEADER, &mut err);
    let credential_revision = required(headers, CREDENTIAL_REVISION_HEADER, &mut err);
    let access_token = required(headers, ACCESS_TOKEN_HEADER, &mut err);
    if let Some(e) = err {
        return Err(e);
    }

    // 4. 字符集与字段形状
    let request_id = request_id.expect("checked above");
    let account_key = account_key.expect("checked above");
    let credential_revision = credential_revision.expect("checked above");
    let access_token = access_token.expect("checked above");
    if request_id.len() > 128
        || !request_id
            .bytes()
            .all(|b| b.is_ascii_graphic() || b == b'-')
    {
        return Err(LocalError::new(
            "invalid_request_id",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "X-CTD-Request-ID failed character/format validation",
        ));
    }
    if account_key.len() > 128 {
        return Err(LocalError::new(
            "invalid_account_key",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "X-CTD-Account-Key exceeds the length limit",
        ));
    }

    // 5. proxy kind/url
    let proxy_kind_raw = header_str(headers, PROXY_KIND_HEADER).unwrap_or("direct");
    let proxy_kind = ProxyKind::parse(proxy_kind_raw).ok_or_else(|| {
        LocalError::new(
            "invalid_proxy_kind",
            ErrorClass::Contract,
            ErrorPhase::ResolveProxy,
            false,
            "unknown X-CTD-Proxy-Kind",
        )
    })?;
    let proxy_url = match proxy_kind {
        ProxyKind::Direct => None,
        _ => {
            let raw = header_str(headers, PROXY_URL_HEADER).ok_or_else(|| {
                LocalError::new(
                    "missing_proxy_url",
                    ErrorClass::Contract,
                    ErrorPhase::ResolveProxy,
                    false,
                    "X-CTD-Proxy-URL is required for non-direct routes",
                )
            })?;
            if raw.len() > 2048 {
                return Err(LocalError::new(
                    "invalid_proxy_url",
                    ErrorClass::Contract,
                    ErrorPhase::ResolveProxy,
                    false,
                    "X-CTD-Proxy-URL exceeds the length limit",
                ));
            }
            Some(SecretString::from(raw.to_string()))
        }
    };

    // 6. session snapshot
    let session = if operation.requires_session() {
        let session_id = required(headers, SESSION_ID_HEADER, &mut err);
        let thread_id = required(headers, THREAD_ID_HEADER, &mut err);
        let window_id = required(headers, WINDOW_ID_HEADER, &mut err);
        if let Some(e) = err {
            return Err(e);
        }
        let snapshot = SessionSnapshot {
            session_id: session_id.expect("checked above"),
            thread_id: thread_id.expect("checked above"),
            window_id: window_id.expect("checked above"),
        };
        validate_session_snapshot(&snapshot)?;
        Some(snapshot)
    } else {
        None
    };

    // F-015：deadline 语义——缺失=operation 默认；格式错/非正=contract
    // error（不静默 300s）；已过=立即过期（server 侧 deadline_exceeded）。
    // R4-M1：admission header 存在时必须解析为显式正整数且不超过上限；
    // 非法值 fail closed（不静默降级到默认）。
    let admission_timeout_ms = match header_str(headers, ADMISSION_TIMEOUT_MS_HEADER) {
        None => None,
        Some(raw) => {
            let millis = raw.trim().parse::<u64>().map_err(|_| {
                LocalError::new(
                    "invalid_admission_timeout",
                    ErrorClass::Contract,
                    ErrorPhase::Validate,
                    false,
                    format!("X-CTD-Admission-Timeout-Ms 非法值 {raw:?}"),
                )
            })?;
            if millis == 0 || millis > 60_000 {
                return Err(LocalError::new(
                    "invalid_admission_timeout",
                    ErrorClass::Contract,
                    ErrorPhase::Validate,
                    false,
                    "X-CTD-Admission-Timeout-Ms 必须在 (0, 60000] ms",
                ));
            }
            Some(millis)
        }
    };
    let deadline: Option<Instant> = match header_str(headers, DEADLINE_UNIX_MS_HEADER) {
        None => None,
        Some(raw) => {
            let ms = raw.trim().parse::<i64>().map_err(|_| {
                LocalError::new(
                    "invalid_deadline",
                    ErrorClass::Contract,
                    ErrorPhase::Validate,
                    false,
                    format!("X-CTD-Deadline-Unix-Ms 非法值 {raw:?}"),
                )
            })?;
            if ms <= 0 {
                return Err(LocalError::new(
                    "invalid_deadline",
                    ErrorClass::Contract,
                    ErrorPhase::Validate,
                    false,
                    "X-CTD-Deadline-Unix-Ms 必须为正毫秒时间戳",
                ));
            }
            let remaining_ms = ms - now_ms() as i64;
            Some(if remaining_ms <= 0 {
                Instant::now() // 已过：立即过期
            } else {
                Instant::now() + std::time::Duration::from_millis(remaining_ms as u64)
            })
        }
    };

    Ok(RequestContext {
        request_id,
        account_key,
        credential_revision,
        access_token: SecretString::from(access_token),
        chatgpt_account_id: header_str(headers, CHATGPT_ACCOUNT_ID_HEADER)
            .filter(|v| !v.is_empty())
            .map(|v| SecretString::from(v.to_string())),
        operation,
        proxy_kind,
        proxy_url,
        session,
        purpose: RequestPurpose::parse(header_str(headers, PURPOSE_HEADER)),
        admission_timeout_ms,
        deadline,
        if_none_match: header_str(headers, IF_NONE_MATCH_HEADER)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string),
        refresh_token: header_str(headers, REFRESH_TOKEN_HEADER)
            .filter(|v| !v.is_empty())
            .map(|v| SecretString::from(v.to_string())),
        agent_private_key: header_str(headers, AGENT_PRIVATE_KEY_HEADER)
            .filter(|v| !v.is_empty())
            .map(|v| SecretString::from(v.to_string())),
        agent_runtime_id: header_str(headers, AGENT_RUNTIME_ID_HEADER)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string),
        oauth_client_id: header_str(headers, OAUTH_CLIENT_ID_HEADER).map(String::from),
        file_id: header_str(headers, FILE_ID_HEADER).map(String::from),
        conversation_id: header_str(headers, CONVERSATION_ID_HEADER).map(String::from),
        attachment_id: header_str(headers, ATTACHMENT_ID_HEADER)
            .filter(|v| !v.is_empty())
            .map(String::from),
        user_id: header_str(headers, USER_ID_HEADER).map(String::from),
        traceparent: header_str(headers, "traceparent")
            .filter(|v| !v.is_empty())
            .map(ToString::to_string),
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 校验 v0_144 session/thread/window（UUIDv7 + window 格式）。
pub fn validate_session_snapshot(snapshot: &SessionSnapshot) -> Result<(), LocalError> {
    codex_core::CodexResponsesMetadata::try_for_transport(
        "00000000-0000-0000-0000-000000000000".to_string(),
        snapshot.session_id.clone(),
        snapshot.thread_id.clone(),
        snapshot.window_id.clone(),
    )
    .map_err(|_| super::error::codes::invalid_session())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::operation::Operation;

    fn base_headers() -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(PROTOCOL_VERSION_HEADER, HeaderValue::from_static("1"));
        headers.insert(REQUEST_ID_HEADER, HeaderValue::from_static("req_123"));
        headers.insert(ACCOUNT_KEY_HEADER, HeaderValue::from_static("ak-hmac"));
        headers.insert(
            CREDENTIAL_REVISION_HEADER,
            HeaderValue::from_static("rev-1"),
        );
        headers.insert(ACCESS_TOKEN_HEADER, HeaderValue::from_static("sk-secret"));
        headers.insert(PROXY_KIND_HEADER, HeaderValue::from_static("direct"));
        headers.insert(
            SESSION_ID_HEADER,
            HeaderValue::from_static("0198f2b0-0000-7000-8000-000000000001"),
        );
        headers.insert(
            THREAD_ID_HEADER,
            HeaderValue::from_static("0198f2b0-0000-7000-8000-000000000002"),
        );
        headers.insert(
            WINDOW_ID_HEADER,
            HeaderValue::from_static("0198f2b0-0000-7000-8000-000000000002:1"),
        );
        headers
    }

    #[test]
    fn parses_valid_request_context() {
        let ctx = parse_request_context(&base_headers(), Operation::Responses).expect("parse");
        assert_eq!(ctx.request_id, "req_123");
        assert_eq!(ctx.proxy_kind, ProxyKind::Direct);
        assert!(ctx.proxy_url.is_none());
        assert_eq!(
            ctx.session.as_ref().expect("session").session_id,
            "0198f2b0-0000-7000-8000-000000000001"
        );
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let mut headers = base_headers();
        headers.insert(PROTOCOL_VERSION_HEADER, HeaderValue::from_static("2"));
        let err = parse_request_context(&headers, Operation::Responses).expect_err("must fail");
        assert_eq!(err.code, "invalid_protocol_version");
    }

    #[test]
    fn rejects_missing_required_header() {
        let mut headers = base_headers();
        headers.remove(ACCESS_TOKEN_HEADER);
        let err = parse_request_context(&headers, Operation::Responses).expect_err("must fail");
        assert_eq!(err.code, "missing_required_header");
    }

    #[test]
    fn rejects_invalid_window_format() {
        let mut headers = base_headers();
        headers.insert(WINDOW_ID_HEADER, HeaderValue::from_static("not-a-window"));
        let err = parse_request_context(&headers, Operation::Responses).expect_err("must fail");
        assert_eq!(err.code, "invalid_session");
    }

    #[test]
    fn rejects_unknown_proxy_kind() {
        let mut headers = base_headers();
        headers.insert(PROXY_KIND_HEADER, HeaderValue::from_static("gopher"));
        let err = parse_request_context(&headers, Operation::Responses).expect_err("must fail");
        assert_eq!(err.code, "invalid_proxy_kind");
    }

    #[test]
    fn models_operation_does_not_require_session() {
        let mut headers = base_headers();
        headers.remove(SESSION_ID_HEADER);
        headers.remove(THREAD_ID_HEADER);
        headers.remove(WINDOW_ID_HEADER);
        let ctx = parse_request_context(&headers, Operation::Models).expect("parse");
        assert!(ctx.session.is_none());
    }

    #[test]
    fn user_id_is_optional_and_never_fails_parse() {
        // 缺省 None：不注入 = 不抓（G4 零开销路径）。
        let ctx = parse_request_context(&base_headers(), Operation::Responses).expect("parse");
        assert!(ctx.user_id.is_none());
        // 注入后按 optional 模式解析（capture-r1）。
        let mut headers = base_headers();
        headers.insert(USER_ID_HEADER, HeaderValue::from_static("3"));
        let ctx = parse_request_context(&headers, Operation::Responses).expect("parse");
        assert_eq!(ctx.user_id.as_deref(), Some("3"));
    }

    #[test]
    fn secret_token_never_leaks_in_debug() {
        let ctx = parse_request_context(&base_headers(), Operation::Responses).expect("parse");
        let debug = format!("{ctx:?}");
        assert!(!debug.contains("sk-secret"));
        assert!(debug.contains("<redacted>") || debug.contains("SecretString"));
    }

    #[test]
    fn socks_kinds_parse_but_are_rejected_later() {
        let mut headers = base_headers();
        headers.insert(PROXY_KIND_HEADER, HeaderValue::from_static("socks5h"));
        headers.insert(
            PROXY_URL_HEADER,
            HeaderValue::from_static("socks5h://proxy:1080"),
        );
        let ctx = parse_request_context(&headers, Operation::Responses).expect("parse");
        assert_eq!(ctx.proxy_kind, ProxyKind::Socks5h);
        assert!(ctx.proxy_url.is_some());
    }
}

#[cfg(test)]
mod p1_header_tests {
    use super::*;

    fn base() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(PROTOCOL_VERSION_HEADER, HeaderValue::from_static("1"));
        headers.insert(REQUEST_ID_HEADER, HeaderValue::from_static("req_p1"));
        headers.insert(ACCOUNT_KEY_HEADER, HeaderValue::from_static("ak"));
        headers.insert(CREDENTIAL_REVISION_HEADER, HeaderValue::from_static("rev"));
        headers.insert(ACCESS_TOKEN_HEADER, HeaderValue::from_static("sk-token"));
        headers.insert(PROXY_KIND_HEADER, HeaderValue::from_static("direct"));
        headers
    }

    #[test]
    fn refresh_token_parsed_as_secret_and_redacted() {
        let mut headers = base();
        headers.insert(
            REFRESH_TOKEN_HEADER,
            HeaderValue::from_static("rt-secret-xyz"),
        );
        headers.insert(
            AGENT_RUNTIME_ID_HEADER,
            HeaderValue::from_static("runtime-1"),
        );
        let ctx = parse_request_context(&headers, Operation::OAuthRefresh).expect("parse");
        assert_eq!(
            ctx.refresh_token.as_ref().expect("token").as_str(),
            "rt-secret-xyz"
        );
        let debug = format!("{ctx:?}");
        assert!(
            !debug.contains("rt-secret-xyz"),
            "refresh token must be redacted"
        );
        assert_eq!(ctx.agent_runtime_id.as_deref(), Some("runtime-1"));
    }

    #[test]
    fn p1_headers_never_leak_via_debug() {
        let mut headers = base();
        headers.insert(
            AGENT_PRIVATE_KEY_HEADER,
            HeaderValue::from_static("pk-super-secret"),
        );
        let ctx = parse_request_context(&headers, Operation::OAuthRefresh).expect("parse");
        let debug = format!("{ctx:?}");
        assert!(!debug.contains("pk-super-secret"));
        assert!(!debug.contains("rt-"));
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    fn ctx_with_deadline(value: &str) -> Result<RequestContext, LocalError> {
        let mut headers = http::HeaderMap::new();
        for (name, val) in [
            (PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION),
            (REQUEST_ID_HEADER, "r1"),
            (ACCOUNT_KEY_HEADER, "k1"),
            (CREDENTIAL_REVISION_HEADER, "rev1"),
            (ACCESS_TOKEN_HEADER, "tok"),
            (PROXY_KIND_HEADER, "direct"),
            (DEADLINE_UNIX_MS_HEADER, value),
        ] {
            headers.insert(name, val.parse().expect("header value"));
        }
        parse_request_context(&headers, crate::contract::operation::Operation::Models)
    }

    #[test]
    fn malformed_deadline_is_contract_error() {
        let err = ctx_with_deadline("not-a-number").expect_err("malformed");
        assert_eq!(err.code, "invalid_deadline");
        let err = ctx_with_deadline("0").expect_err("zero");
        assert_eq!(err.code, "invalid_deadline");
        let err = ctx_with_deadline("-5").expect_err("negative");
        assert_eq!(err.code, "invalid_deadline");
    }

    #[test]
    fn past_deadline_expires_immediately() {
        let past_ms = now_ms() - 5000;
        let ctx = ctx_with_deadline(&past_ms.to_string()).expect("past deadline parses");
        let deadline = ctx.deadline.expect("deadline present");
        assert!(
            deadline <= Instant::now() + std::time::Duration::from_millis(100),
            "已过 deadline 必须立即过期"
        );
    }

    #[test]
    fn invalid_admission_timeout_fails_closed() {
        for bad in ["abc", "0", "-5", "99999999999999999999", "1.5"] {
            let mut headers = http::HeaderMap::new();
            for (name, val) in [
                (PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION),
                (REQUEST_ID_HEADER, "r1"),
                (ACCOUNT_KEY_HEADER, "k1"),
                (CREDENTIAL_REVISION_HEADER, "rev1"),
                (ACCESS_TOKEN_HEADER, "tok"),
                (PROXY_KIND_HEADER, "direct"),
                (ADMISSION_TIMEOUT_MS_HEADER, bad),
            ] {
                headers.insert(name, val.parse().expect("header value"));
            }
            let err =
                parse_request_context(&headers, crate::contract::operation::Operation::Models)
                    .expect_err(bad);
            assert_eq!(err.code, "invalid_admission_timeout", "{}", bad);
        }
        // 合法值通过。
        let mut headers = http::HeaderMap::new();
        for (name, val) in [
            (PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION),
            (REQUEST_ID_HEADER, "r1"),
            (ACCOUNT_KEY_HEADER, "k1"),
            (CREDENTIAL_REVISION_HEADER, "rev1"),
            (ACCESS_TOKEN_HEADER, "tok"),
            (PROXY_KIND_HEADER, "direct"),
            (ADMISSION_TIMEOUT_MS_HEADER, "100"),
        ] {
            headers.insert(name, val.parse().expect("header value"));
        }
        let ctx = parse_request_context(&headers, crate::contract::operation::Operation::Models)
            .expect("valid");
        assert_eq!(ctx.admission_timeout_ms, Some(100));
    }

    #[test]
    fn missing_deadline_defaults_to_none() {
        let mut headers = http::HeaderMap::new();
        for (name, val) in [
            (PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION),
            (REQUEST_ID_HEADER, "r1"),
            (ACCOUNT_KEY_HEADER, "k1"),
            (CREDENTIAL_REVISION_HEADER, "rev1"),
            (ACCESS_TOKEN_HEADER, "tok"),
            (PROXY_KIND_HEADER, "direct"),
        ] {
            headers.insert(name, val.parse().expect("header value"));
        }
        let ctx = parse_request_context(&headers, crate::contract::operation::Operation::Models)
            .expect("parsed");
        assert!(ctx.deadline.is_none());
    }

    #[test]
    fn future_deadline_kept() {
        let future_ms = now_ms() + 60_000;
        let ctx = ctx_with_deadline(&future_ms.to_string()).expect("future deadline");
        assert!(ctx.deadline.is_some());
    }
}
