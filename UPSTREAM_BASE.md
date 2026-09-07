# UPSTREAM_BASE（T-002 / CP-002 冻结）

官方 exact-tag fork 基准（本仓库 branch `transportd/v0.144.1` 的 parent）。

| 项 | 值 |
|---|---|
| upstream remote | `https://github.com/openai/codex` |
| tag | `rust-v0.144.1` |
| peeled commit | `44918ea10c0f99151c6710411b4322c2f5c96bea` |
| `codex-rs/Cargo.lock` SHA-256 | `175793a40a3147db1fee08fd9db0acc59312c344b3513dd7ee316f5446d8119e` |
| toolchain | `1.95.0`（`codex-rs/rust-toolchain.toml`，含 clippy/rustfmt/rust-src） |
| license | Apache-2.0 |

可复现命令：

```bash
git ls-remote https://github.com/openai/codex 'refs/tags/rust-v0.144.1^{}'
# 44918ea10c0f99151c6710411b4322c2f5c96bea
shasum -a 256 codex-rs/Cargo.lock
# 175793a40a3147db1fee08fd9db0acc59312c344b3513dd7ee316f5446d8119e
```

本仓库只消费不可变 upstream 输入；任何 upstream 变更先更新本文件并重跑
CP-002 gate。
