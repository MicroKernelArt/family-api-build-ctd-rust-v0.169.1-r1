//! T3 负向（红线，capture-r1 F3）：带 `x-ctd-user-id` 的入站 context 走
//! 真实出站构造（`execute_responses_raw` → 官方 client → mock 上游），
//! mock 上游观测到的请求头里任何 `x-ctd-*` 一律缺席。
//!
//! 隔离：`CTD_TEST_UPSTREAM_BASE_URL` 是进程级 override（仅 test-upstream
//! feature 生效，TLS-CTD-001 §15.2 既有测试 harness）。为不污染同进程的
//! 其他测试（如官方 base URL 断言），真实请求在自孵化子进程中执行：
//! 父进程用 std::TcpListener 起 mock，`current_exe()` 重入测试二进制并只
//! 跑 worker 测试（--exact），env 只注入子进程——父进程零 env 变更。

#![cfg(feature = "test-upstream")]

use std::io::{Read, Write};
use std::process::Command;

use bytes::Bytes;
use futures::StreamExt;
use codex_transportd::contract::RequestContext;
use codex_transportd::contract::headers::SessionSnapshot;
use codex_transportd::observability::Metrics;
use codex_transportd::redaction::SecretString;
use codex_transportd::transport::client_pool::ClientPool;
use codex_transportd::transport::responses::execute_responses_raw;

/// worker 测试名（子进程 --exact 过滤）。
const WORKER_TEST: &str = "capture_t3_mock_upstream_worker";

#[test]
fn mock_upstream_never_observes_x_ctd_headers() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let server = std::thread::spawn(move || serve_one(&listener));

    let exe = std::env::current_exe().expect("current test binary");
    let out = Command::new(exe)
        .args([WORKER_TEST, "--exact", "--test-threads=1"])
        .env("CTD_TEST_UPSTREAM_BASE_URL", format!("http://{addr}"))
        .env("CTD_T3_CHILD", "1")
        .output()
        .expect("spawn worker child");
    assert!(
        out.status.success(),
        "worker failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let (head, body) = server.join().expect("mock server thread");
    let request_line = head.split("\r\n").next().unwrap_or_default();
    assert!(
        request_line.starts_with("POST /responses"),
        "unexpected request line: {request_line}"
    );
    let mut saw_session = false;
    let mut saw_bearer = false;
    for line in head.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        let (name, _) = line.split_once(':').unwrap_or((line, ""));
        let name = name.trim().to_ascii_lowercase();
        // 红线（G6/T3）：任何 x-ctd-* 内部 header 都不得出现在真实上游请求。
        assert!(
            !name.starts_with("x-ctd-"),
            "internal header namespace leaked to the real outbound request: {name}"
        );
        if name == "session-id" {
            saw_session = true;
        }
        if name == "authorization" {
            saw_bearer = true;
        }
    }
    assert!(
        saw_session,
        "outbound must carry official session headers: {head}"
    );
    assert!(
        saw_bearer,
        "outbound must carry the official bearer auth path: {head}"
    );
    assert!(
        body.windows(b"capture-t3-marker".len())
            .any(|w| w == b"capture-t3-marker"),
        "request body must reach the mock upstream byte-true"
    );
}

/// 最小 HTTP/1.1 mock 上游：读取一个请求（head + content-length body），
/// 回一个最小 SSE 形态 200，返回 (head, body) 供断言。
fn serve_one(listener: &std::net::TcpListener) -> (String, Vec<u8>) {
    let (mut sock, _) = listener.accept().expect("accept");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut chunk).expect("read request head");
        assert!(n > 0, "eof before request head completed");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        assert!(buf.len() < 64 * 1024, "request head exceeds bound");
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let content_length = head
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    assert!(
        content_length <= 8 * 1024 * 1024,
        "content-length exceeds bound"
    );
    while buf.len() < head_end + content_length {
        let n = sock.read(&mut chunk).expect("read request body");
        assert!(n > 0, "eof before request body completed");
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[head_end..head_end + content_length].to_vec();
    let resp = b"retry: 0\n\n";
    sock.write_all(
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
            resp.len()
        )
        .as_bytes(),
    )
    .expect("write response head");
    sock.write_all(resp).expect("write response body");
    (head, body)
}

/// 子进程内执行真实出站构造；env 由父进程注入（本测试自身不变更 env）。
#[tokio::test]
async fn capture_t3_mock_upstream_worker() {
    if std::env::var("CTD_T3_CHILD").is_err() {
        // 主套件直跑占位：真实逻辑仅由父测试 spawn 的 --exact 子进程执行。
        return;
    }
    let base = std::env::var("CTD_TEST_UPSTREAM_BASE_URL").unwrap_or_default();
    assert!(
        !base.is_empty(),
        "run via mock_upstream_never_observes_x_ctd_headers (needs CTD_TEST_UPSTREAM_BASE_URL)"
    );
    assert_eq!(
        std::env::var("CTD_T3_CHILD").as_deref(),
        Ok("1"),
        "worker must run as the spawned child, never in-process"
    );
    // 双保险：override 只允许指向 loopback，绝无打到官方端点的可能。
    let parsed = url::Url::parse(&base).expect("base url parses");
    assert_eq!(parsed.scheme(), "http", "mock is plain loopback http");
    assert!(
        matches!(
            parsed.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
        ),
        "mock base must be loopback, got {}",
        base
    );

    let ctx = RequestContext {
        request_id: "req_capture_t3".to_string(),
        account_key: "acct".to_string(),
        credential_revision: "rev-1".to_string(),
        access_token: SecretString::new("test-token"),
        chatgpt_account_id: Some(SecretString::new("acc-1")),
        proxy_kind: codex_transportd::contract::ProxyKind::Direct,
        session: Some(SessionSnapshot {
            session_id: "0198f2b0-0000-7000-8000-000000000001".to_string(),
            thread_id: "0198f2b0-0000-7000-8000-000000000002".to_string(),
            window_id: "0198f2b0-0000-7000-8000-000000000002:1".to_string(),
        }),
        // T3 断言对象：capture 白名单身份进入 context。
        user_id: Some("3".to_string()),
        ..Default::default()
    };
    let pool = ClientPool::new(std::sync::Arc::new(Metrics::default()));
    let body: Bytes = serde_json::json!({
        "model": "gpt-5.2-capture-t3",
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "capture-t3-marker"}],
        }],
    })
    .to_string()
    .into_bytes()
    .into();

    let resp = execute_responses_raw(&pool, "installation-1", &ctx, body)
        .await
        .expect("real outbound construction must execute");
    assert_eq!(resp.status, 200, "mock upstream answered 200");
    let mut received = Vec::new();
    let mut stream = resp.body;
    while let Some(chunk) = stream.next().await {
        received.extend_from_slice(&chunk.expect("mock stream chunk"));
    }
    assert_eq!(received, b"retry: 0\n\n", "mock body forwards byte-true");
}
