//! transport 层：官方 adapter、账号级 proxy、隔离 client pool 与各
//! operation 的原始执行。

pub mod agent;
pub mod auth;
pub mod auxiliary;
pub mod client_pool;
pub mod compact;
pub mod env_mutex;
pub mod files;
pub mod metadata;
pub mod official_adapter;
pub mod proxy;
pub mod responses;
pub mod responses_v3;
pub mod url_policy;
pub mod websocket;
