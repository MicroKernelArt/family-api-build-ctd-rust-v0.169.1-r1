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
