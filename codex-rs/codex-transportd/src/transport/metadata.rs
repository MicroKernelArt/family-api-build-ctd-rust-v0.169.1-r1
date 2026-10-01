//! Reserved metadata 边界（TLS-CTD-001 §11.5 / FR-010 / INV-005）。
//!
//! Go 提交完成业务转换的 JSON，但以下 reserved 字段只由 Rust 从官方
//! generator 产生：根对象或 `client_metadata` 中出现 reserved duplicate key
//! 一律 fail closed（§11.5.2），不得“尽量清理后继续”（§16.5）。

#[cfg(test)]
#[path = "metadata_reserved_keys_tests.rs"]
mod reserved_keys_tests;

use serde_json::Map;
use serde_json::Value;

use crate::contract::SessionSnapshot;
use crate::contract::error::ErrorClass;
use crate::contract::error::ErrorPhase;
use crate::contract::error::LocalError;

/// 根对象 reserved keys（Rust 官方投影独有）。
pub const RESERVED_ROOT_KEYS: &[&str] = &[
    "session-id",
    "thread-id",
    "x-codex-window-id",
    "x-codex-installation-id",
    "originator",
    "user-agent",
    "openai-beta",
    "x-codex-turn-state",
    "x-openai-subagent",
    "x-codex-turn-metadata",
    "x-codex-parent-thread-id",
];

/// `client_metadata` 内 reserved keys（R-METADATA-001）：直接采用 core
/// 官方列表（`codex_core::RESERVED_METADATA_KEYS`，含
/// `x-codex-parent-thread-id`）为单一事实来源——core 新增保留键时
/// sidecar 自动 fail closed，不再维护第二份可漂移副本。
pub const RESERVED_METADATA_KEYS: &[&str] = codex_core::RESERVED_METADATA_KEYS;

/// 校验请求 JSON 为对象，且根对象/reserved 键无冲突。
/// 严格重复 key 检测（round-3 H2）：递归遍历 JSON，任何对象出现重复
/// key 立即失败——serde_json 默认后者覆盖，会静默改变请求数据。
pub fn reject_duplicate_keys(raw: &str) -> Result<(), LocalError> {
    use serde::de::DeserializeSeed;
    use serde::de::MapAccess;
    use serde::de::Visitor;
    use std::collections::HashSet;

    struct DupGuard;
    impl<'de> Visitor<'de> for DupGuard {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("JSON value without duplicate keys")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut seen = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !seen.insert(key.clone()) {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate JSON key: {key}"
                    )));
                }
                map.next_value_seed(DupGuard)?;
            }
            Ok(())
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            while seq.next_element_seed(DupGuard)?.is_some() {}
            Ok(())
        }
        // 标量叶子：无重复 key 语义，直接通过。
        fn visit_bool<E>(self, _v: bool) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_i64<E>(self, _v: i64) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_u64<E>(self, _v: u64) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_f64<E>(self, _v: f64) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_str<E>(self, _v: &str) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_string<E>(self, _v: String) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(())
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            // R4-H1：普通 JSON null 必须通过通用扫描（deserialize_any 对
            // null 调 visit_unit；client_metadata:null 由语义层拒绝）。
            Ok(())
        }
        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(DupGuard)
        }
    }
    impl<'de> DeserializeSeed<'de> for DupGuard {
        type Value = ();
        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(DupGuard)
        }
    }
    // R4-H1：顶层与嵌套统一使用 DupGuard（完整支持 map/seq/标量/unit）。
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    DupGuard.deserialize(&mut deserializer).map_err(|err| {
        LocalError::new(
            "invalid_json",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            format!("request body JSON 校验失败: {err}"),
        )
    })?;
    Ok(())
}

pub fn validate_body_object(body: &Value) -> Result<(), LocalError> {
    let obj = body.as_object().ok_or_else(|| {
        LocalError::new(
            "invalid_json",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "request body must be a JSON object",
        )
    })?;
    for key in obj.keys() {
        if RESERVED_ROOT_KEYS.contains(&key.as_str()) {
            return Err(LocalError::new(
                "reserved_metadata_conflict",
                ErrorClass::Contract,
                ErrorPhase::Validate,
                false,
                format!("reserved root key present: {key}"),
            ));
        }
    }
    Ok(())
}

/// 提取并校验 `client_metadata`（若存在），返回非 reserved 的 extra 键值。
/// F-010：保留任意 JSON 类型（number/bool/null/array/nested object），
/// 不再把非 string 静默丢弃（Go 侧已无损传 JSON）。
pub fn extract_client_metadata(
    obj: &Map<String, Value>,
) -> Result<Vec<(String, Value)>, LocalError> {
    let Some(value) = obj.get("client_metadata") else {
        return Ok(Vec::new());
    };
    // round-3 H2：client_metadata 存在但非 object → fail closed
    // （null/数组/标量不得静默当作空 metadata 处理）。
    let Value::Object(metadata) = value else {
        return Err(LocalError::new(
            "invalid_client_metadata",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "client_metadata must be an object",
        ));
    };
    let mut extra = Vec::new();
    for (key, value) in metadata {
        if RESERVED_METADATA_KEYS.contains(&key.as_str()) {
            return Err(LocalError::new(
                "reserved_metadata_conflict",
                ErrorClass::Contract,
                ErrorPhase::Validate,
                false,
                format!("reserved client_metadata key present: {key}"),
            ));
        }
        extra.push((key.clone(), value.clone()));
    }
    Ok(extra)
}

/// 用官方投影替换 body 的 `client_metadata`（保留非 reserved extra，
/// 原 JSON 类型无损，F-010）。
pub fn inject_official_client_metadata(
    obj: &mut Map<String, Value>,
    extra: Vec<(String, Value)>,
    projection_client_metadata: std::collections::HashMap<String, String>,
) {
    let mut merged: Map<String, Value> = projection_client_metadata
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    for (key, value) in extra {
        merged.insert(key, value);
    }
    obj.insert("client_metadata".to_string(), Value::Object(merged));
}

/// `x-codex-turn-metadata` 在 body 中的 canonical 键名
/// （官方 `client_metadata()` 用它写入 body；compatibility header 只是投影）。
pub const TURN_METADATA_CLIENT_METADATA_KEY: &str = "x-codex-turn-metadata";

/// 把承载的 `x-codex-turn-metadata` 补进 body 的 canonical 位。
///
/// 官方客户端 HTTP 同时写两处：body `client_metadata["x-codex-turn-metadata"]`
/// （canonical）与 compatibility header。sidecar 的 projection 只由 session
/// snapshot 合成，`has_turn_metadata()` 恒为 false；Go 侧 reserved 剥离又会删掉
/// 客户端原始值 ⇒ 这一位只有承载通道能补，否则 rust_sidecar 的 body 比 R-Go
/// 直连少一个 canonical 字段。
///
/// 值按字符串原样写入（官方投影同样是 JSON 字符串），已存在时不覆盖。
/// 已知近似：承载的是 **header** 值；若客户端 header 与 body 两处取值不一致，
/// rust_sidecar 采用 header 值（R-Go 采用 body 值）。
pub fn inject_carried_turn_metadata(
    obj: &mut Map<String, Value>,
    carried: &std::collections::BTreeMap<String, String>,
) {
    let Some(raw) = carried.get(TURN_METADATA_CLIENT_METADATA_KEY) else {
        return;
    };
    let Some(Value::Object(metadata)) = obj.get_mut("client_metadata") else {
        return;
    };
    if metadata.contains_key(TURN_METADATA_CLIENT_METADATA_KEY) {
        return;
    }
    metadata.insert(
        TURN_METADATA_CLIENT_METADATA_KEY.to_string(),
        Value::String(raw.clone()),
    );
}

/// 官方 `client_metadata` 中承载通道**无权交付**（[`RESERVED_METADATA_KEYS`]
/// 拒绝）而官方必发的键名。
pub const TURN_ID_KEY: &str = "turn_id";
pub const ROOT_TURN_ID_KEY: &str = "root_turn_id";
pub const GUARDIAN_CREDITS_REQUESTED_KEY: &str = "guardian_credits_requested";
pub const MCP_ATTRIBUTION_KEY: &str = "mcp_attribution";

/// 官方「无 MCP 归因」的序列化常量
/// （`codex_protocol::mcp::McpAttribution::default()`，
/// `protocol/src/mcp.rs:67-72,115-126`：`status = none`，`sources` 为空且
/// `skip_serializing_if` ⇒ 只剩 `{"status":"none"}`；`docs/21 §5.3` 实测同值）。
pub const MCP_ATTRIBUTION_NONE: &str = r#"{"status":"none"}"#;

/// 补齐官方 `client_metadata` 中由 ctd 派生、承载通道无权交付的 4 个键
/// （`docs/22 §6` 项 #8/#10/#12/#14/#16）。
///
/// 各键的真值来源：
/// - `turn_id` / `root_turn_id`：**派生**。官方 I7 要求它们与请求头
///   `x-codex-turn-metadata` 同源（`docs/21 §6`）；承载的 turn-metadata 就是
///   该权威值（`inject_carried_turn_metadata` 已写入 canonical 位），此处直接
///   取其中同名字段。缺失该头（或其中不含对应键，如 `request_kind=prewarm`）
///   ⇒ 不补，保持与官方「无该字段就不发」一致。
/// - `guardian_credits_requested`：**派生**。官方谓词
///   （`core/src/client.rs:1126-1158` `set_guardian_metadata`）为
///   「非 guardian reviewer ∧ 非 basic session source ∧ ChatGPT 凭据 ∧
///   codex 后端 ∧ provider 支持 codex 后端路由」。v1 下 provider 恒为
///   chatgpt codex base（`CHATGPT_CODEX_BASE_URL`），auth 恒为
///   Bearer + ChatGPT-Account-ID，guardian reviewer 标记头
///   （`x-codex-guardian`）不在承载白名单内 ⇒ 恒 false ⇒ 谓词收敛为
///   「是否 ChatGPT 账号凭据」。
/// - `mcp_attribution`：**常量近似**。官方值是客户端会话的累积 MCP 归因，
///   sidecar 侧无该状态（Go 侧同样无，`docs/19 §17`）⇒ 按官方「无 MCP 结果」
///   形态写入。★ 已登记的近似：若客户端本次会话用过 MCP，该键会与真实归因
///   不符（该键 body-only、model-invisible）。
pub fn inject_derived_client_metadata(
    obj: &mut Map<String, Value>,
    ctx: &crate::contract::RequestContext,
) {
    let Some(Value::Object(metadata)) = obj.get_mut("client_metadata") else {
        return;
    };
    let turn_metadata = metadata
        .get(TURN_METADATA_CLIENT_METADATA_KEY)
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(raw) = turn_metadata
        && let Ok(Value::Object(turn_metadata)) = serde_json::from_str::<Value>(&raw)
    {
        for key in [TURN_ID_KEY, ROOT_TURN_ID_KEY] {
            if let Some(value @ Value::String(_)) = turn_metadata.get(key) {
                metadata.insert(key.to_string(), value.clone());
            }
        }
    }
    if ctx.chatgpt_account_id.is_some() {
        metadata.insert(
            GUARDIAN_CREDITS_REQUESTED_KEY.to_string(),
            Value::String("true".to_string()),
        );
    }
    metadata.insert(
        MCP_ATTRIBUTION_KEY.to_string(),
        Value::String(MCP_ATTRIBUTION_NONE.to_string()),
    );
}

/// 构造官方 metadata 投影（UUIDv7 校验在入口完成）。
pub fn build_projection(
    installation_id: &str,
    session: &SessionSnapshot,
) -> Result<codex_core::CodexResponsesMetadata, LocalError> {
    codex_core::CodexResponsesMetadata::try_for_transport(
        installation_id.to_string(),
        session.session_id.clone(),
        session.thread_id.clone(),
        session.window_id.clone(),
    )
    .map_err(|_| {
        LocalError::new(
            "invalid_session",
            ErrorClass::Contract,
            ErrorPhase::Validate,
            false,
            "session snapshot failed official projection validation",
        )
    })
}

/// 数值字面量安全（round-3 H2）：serde_json arbitrary_precision 下
/// Number 保留原始字面量；这里按**字面量形态**判定：
/// - 小数/指数（含 . 或 e/E）→ 允许（0.5、1e10 是合法 JSON，官方接受）；
/// - 纯整数 → 必须在 u64/i64 可表示范围（超范围字面量拒绝，防止
///   经 f64 往返丢精度）。
pub fn big_int_roundtrip_safe(body: &Value) -> bool {
    fn walk(value: &Value) -> bool {
        match value {
            Value::Number(number) => {
                let raw = number.to_string();
                if raw.contains('.') || raw.contains('e') || raw.contains('E') {
                    true // 小数/指数合法
                } else {
                    number.as_u64().is_some() || number.as_i64().is_some()
                }
            }
            Value::Array(items) => items.iter().all(walk),
            Value::Object(map) => map.values().all(walk),
            _ => true,
        }
    }
    walk(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reserved_root_key_fails_closed() {
        let body = json!({"model": "gpt-5-codex", "session-id": "fake"});
        let err = validate_body_object(&body).expect_err("reserved root key");
        assert_eq!(err.code, "reserved_metadata_conflict");
    }

    #[test]
    fn reserved_metadata_key_fails_closed() {
        let body = json!({"client_metadata": {"installation_id": "fake"}});
        validate_body_object(&body).expect("object ok");
        let err = extract_client_metadata(body.as_object().unwrap()).expect_err("reserved key");
        assert_eq!(err.code, "reserved_metadata_conflict");
    }

    #[test]
    fn non_reserved_metadata_passes_through() {
        let body = json!({"client_metadata": {"custom_key": "v"}});
        let extra = extract_client_metadata(body.as_object().unwrap()).expect("extra");
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "custom_key");
        assert_eq!(extra[0].1, serde_json::json!("v"));
    }

    #[test]
    fn non_object_body_rejected() {
        let err = validate_body_object(&json!([1, 2, 3])).expect_err("array");
        assert_eq!(err.code, "invalid_json");
    }

    #[test]
    fn big_integers_are_not_roundtripped_through_f64() {
        assert!(big_int_roundtrip_safe(&json!(9007199254740993u64))); // 2^53+1
        assert!(big_int_roundtrip_safe(&json!(u64::MAX)));
        // 超过 u64 的字面量必须被拒绝而不是截断。
        let huge = "18446744073709551616";
        let parsed: Value = serde_json::from_str(huge).expect("parse");
        assert!(!big_int_roundtrip_safe(&parsed));
    }

    #[test]
    fn fractional_and_exponent_literals_allowed() {
        // round-3 H2：小数/指数是合法 JSON，不得误拒。
        for raw in ["0.5", "-1.25", "1e10", "1.5e-3", "6.02e23"] {
            let parsed: Value = serde_json::from_str(raw).expect(raw);
            assert!(big_int_roundtrip_safe(&parsed), "字面量 {raw} 必须允许");
        }
        // 大整数保真（arbitrary_precision 保留字面量）。
        let parsed: Value = serde_json::from_str("9007199254740993").expect("parse");
        assert!(big_int_roundtrip_safe(&parsed));
        assert_eq!(parsed.to_string(), "9007199254740993");
    }

    #[test]
    fn inject_replaces_reserved_metadata() {
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), Value::String("gpt-5-codex".into()));
        inject_official_client_metadata(
            &mut obj,
            vec![("custom".to_string(), serde_json::json!("x"))],
            [("session_id".to_string(), "s".to_string())]
                .into_iter()
                .collect(),
        );
        let metadata = obj.get("client_metadata").unwrap().as_object().unwrap();
        assert_eq!(metadata.get("session_id").unwrap(), "s");
        assert_eq!(metadata.get("custom").unwrap(), "x");
    }

    /// 官方 exec 0.159.2 首条 POST 的 `client_metadata` 键集合
    /// （`baseline/official-1592.json:body.client_metadata_keys`）。
    const OFFICIAL_CLIENT_METADATA_KEYS: [&str; 9] = [
        "guardian_credits_requested",
        "mcp_attribution",
        "root_turn_id",
        "session_id",
        "thread_id",
        "turn_id",
        "x-codex-installation-id",
        "x-codex-turn-metadata",
        "x-codex-window-id",
    ];

    fn derived_fixture_ctx(chatgpt: bool) -> crate::contract::RequestContext {
        crate::contract::RequestContext {
            chatgpt_account_id: chatgpt
                .then(|| crate::redaction::SecretString::new("acc-1")),
            ..Default::default()
        }
    }

    /// 复刻 `execute_responses_raw` 的 body 组装链（投影 → 承载 turn-metadata
    /// → 派生键），断言键集合与官方 9 键逐字相等。
    fn assembled_client_metadata(
        carried: &std::collections::BTreeMap<String, String>,
        chatgpt: bool,
    ) -> Value {
        let session = crate::contract::headers::SessionSnapshot {
            session_id: "0198f2b0-0000-7000-8000-000000000001".to_string(),
            thread_id: "0198f2b0-0000-7000-8000-000000000002".to_string(),
            window_id: "0198f2b0-0000-7000-8000-000000000002:1".to_string(),
        };
        let projection = build_projection("installation-1", &session).expect("projection");
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), Value::String("gpt-5.6-sol".into()));
        inject_official_client_metadata(
            &mut obj,
            Vec::new(),
            projection.client_metadata(/*include_internal*/ true),
        );
        inject_carried_turn_metadata(&mut obj, carried);
        inject_derived_client_metadata(&mut obj, &derived_fixture_ctx(chatgpt));
        obj.get("client_metadata").cloned().expect("client_metadata")
    }

    #[test]
    fn derived_keys_complete_official_client_metadata_key_set() {
        let carried = std::collections::BTreeMap::from([(
            TURN_METADATA_CLIENT_METADATA_KEY.to_string(),
            r#"{"request_kind":"turn","turn_id":"0198f2b0-0000-7000-8000-000000000003","root_turn_id":"0198f2b0-0000-7000-8000-000000000003","session_id":"0198f2b0-0000-7000-8000-000000000001"}"#
                .to_string(),
        )]);
        let metadata = assembled_client_metadata(&carried, /*chatgpt*/ true);
        let mut keys: Vec<&str> = metadata.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = OFFICIAL_CLIENT_METADATA_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
        assert_eq!(metadata["guardian_credits_requested"], "true");
        assert_eq!(metadata["mcp_attribution"], r#"{"status":"none"}"#);
        assert_eq!(metadata["turn_id"], "0198f2b0-0000-7000-8000-000000000003");
        assert_eq!(
            metadata["root_turn_id"],
            "0198f2b0-0000-7000-8000-000000000003"
        );
    }

    #[test]
    fn turn_ids_are_derived_from_the_carried_turn_metadata() {
        // 官方 I7：`client_metadata.{turn_id,root_turn_id}` 必须与请求头
        // `x-codex-turn-metadata` 同源（docs/21 §6）。
        let carried = std::collections::BTreeMap::from([(
            TURN_METADATA_CLIENT_METADATA_KEY.to_string(),
            r#"{"request_kind":"turn","turn_id":"turn-a","root_turn_id":"root-a"}"#.to_string(),
        )]);
        let metadata = assembled_client_metadata(&carried, true);
        assert_eq!(metadata["turn_id"], "turn-a");
        assert_eq!(metadata["root_turn_id"], "root-a");

        // 无承载 turn-metadata（或其中不含该字段，如 prewarm）⇒ 不补，
        // 与官方「无该字段就不发」一致。
        let metadata = assembled_client_metadata(&Default::default(), true);
        assert!(metadata.get("turn_id").is_none());
        assert!(metadata.get("root_turn_id").is_none());
    }

    #[test]
    fn guardian_credits_follows_the_official_chatgpt_auth_predicate() {
        // 官方 `set_guardian_metadata` 要求 ChatGPT 凭据（core/src/client.rs:1126-1158）。
        let chatgpt = assembled_client_metadata(&Default::default(), /*chatgpt*/ true);
        assert_eq!(chatgpt["guardian_credits_requested"], "true");
        let api_key = assembled_client_metadata(&Default::default(), /*chatgpt*/ false);
        assert!(api_key.get("guardian_credits_requested").is_none());
    }
}

#[cfg(test)]
mod round3_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_keys_rejected_recursively() {
        if let Err(err) = reject_duplicate_keys(r#"{"a":1,"b":2}"#) {
            panic!("unexpected: {}", err.message);
        }
        let err = reject_duplicate_keys(r#"{"a":1,"a":2}"#).expect_err("dup root");
        assert_eq!(err.code, "invalid_json");
        assert!(err.message.contains("duplicate JSON key"));
        // 嵌套对象/数组内重复也拒绝。
        let err = reject_duplicate_keys(r#"{"x":{"y":1,"y":2}}"#).expect_err("dup nested");
        assert_eq!(err.code, "invalid_json");
        let err = reject_duplicate_keys(r#"{"x":[{"z":1,"z":2}]}"#).expect_err("dup array");
        assert_eq!(err.code, "invalid_json");
    }

    #[test]
    fn client_metadata_null_fails_closed() {
        let body = json!({"client_metadata": null, "model": "x"});
        let err = extract_client_metadata(body.as_object().unwrap()).expect_err("null metadata");
        assert_eq!(err.code, "invalid_client_metadata");
        let body = json!({"client_metadata": [1, 2], "model": "x"});
        let err = extract_client_metadata(body.as_object().unwrap()).expect_err("array metadata");
        assert_eq!(err.code, "invalid_client_metadata");
    }

    #[test]
    fn fractional_numbers_accepted_end_to_end() {
        // {"temperature":0.5} 不得被误拒（round-3 对抗场景）。
        let raw = r#"{"model":"x","temperature":0.5,"top_p":1e-2}"#;
        let value: Value = serde_json::from_str(raw).expect("parse");
        assert!(big_int_roundtrip_safe(&value));
        // 超 u64 整数字面量仍拒绝。
        let huge: Value = serde_json::from_str("18446744073709551616").expect("parse");
        assert!(!big_int_roundtrip_safe(&huge));
    }
}

#[cfg(test)]
mod round4_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_null_passes_general_scanner() {
        // R4-H1：普通 JSON null（根/对象值/嵌套/数组元素）必须通过。
        for raw in [
            "null",
            r#"{"previous_response_id": null}"#,
            r#"{"a": {"b": null}}"#,
            r#"{"a": [null, 1]}"#,
        ] {
            reject_duplicate_keys(raw)
                .unwrap_or_else(|e| panic!("{raw} 不得被通用扫描拒绝: {}", e.message));
        }
        // client_metadata: null 仍由语义层拒绝。
        let body = json!({"client_metadata": null, "model": "x"});
        let err = extract_client_metadata(body.as_object().unwrap()).expect_err("null metadata");
        assert_eq!(err.code, "invalid_client_metadata");
        // null 与重复 key 组合仍拒绝。
        let err = reject_duplicate_keys(r#"{"a": null, "a": 1}"#).expect_err("dup with null");
        assert_eq!(err.code, "invalid_json");
    }
}
