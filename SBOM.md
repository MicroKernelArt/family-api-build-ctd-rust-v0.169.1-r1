# SBOM 与供应链证据（T-011 / TLS-CTD-001 §19）

## 固定输入链

| 输入 | 值 | 证据 |
|---|---|---|
| upstream repo | `https://github.com/openai/codex` | UPSTREAM_BASE.md |
| upstream tag | `rust-v0.159.2` | `git ls-remote refs/tags/rust-v0.159.2^{}` |
| upstream commit | `ff6aec96948b70d94983af2641a6b67c94faeff5` | 同上 |
| upstream Cargo.lock SHA-256 | `4fab6fe99180e2b8f305bd4b33c1b4bd825e3fa4cdef1045be750843bd3d431e` | `shasum -a 256` |
| fork branch | （不声明：不存在该名字的分支；实际构建 ref 见 receipt 的 review_commit） | — |
| fork commit | `（构建时由 build.rs 注入，见 /info fork_commit）`（构建时自动注入，/info fork_commit 一致） | git + build.rs |
| fork Cargo.lock SHA-256 | `（构建时由 build.rs 计算，见 /info fork_cargo_lock_sha256）`（构建时自动计算，/info fork_cargo_lock_sha256 一致） | build.rs sha2 |
| toolchain | `1.95.0`（clippy/rustfmt/rust-src） | codex-rs/rust-toolchain.toml |
| target | `x86_64-unknown-linux-gnu` | build.rs 注入 |
| license | Apache-2.0 | 官方 LICENSE + NOTICE |
| base image | `debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258`（amd64） | `codex-rs/codex-transportd/BUILD_PROOF.md` + `codex-rs/codex-transportd/Dockerfile`（运行阶段 `FROM`；`UPSTREAM_BASE.md` 不含该 digest） |
| build-proof binary SHA-256 | CP-002 历史记录 / 非本线声明：`4ca3dc86253573cd6f53fee398cb16a446cda139128eda1f362ea7af1dd4f43c`（旧 rust-v0.156.1 线的 CP-002 build-proof 摘要，**不是**本线产物的摘要） | `codex-rs/codex-transportd/BUILD_PROOF.md`（Build-proof digest 段对本线仍为 `TBD（构建后回填 …）`，构建后才产生本线值） |
| final release candidate digest | TBD（T-010 wire gate 通过后由 CI 产出并绑定） | CI artifact |

## 发布产物

- binary SHA-256（上表）+ image digest（`CODEX_TRANSPORTD_DIGEST` 部署绑定）；
- SBOM：由 CI 对最终 release candidate 生成（cargo sbom/trivy 工具链产出归档）；
- license report：workspace 依赖许可证（Apache-2.0 主许可，无新增第三方网络栈）；
- `/info` 与 readiness 校验：`codex_commit`/`cargo_lock_sha256`/image digest
  精确匹配（SC-003）。

## 规则

- 只 promote 已通过 wire 对照的 immutable digest（§19.1）；
- 任何 rebuild/digest 变化使对应 wire 证据失效并重跑；
- CI 从 clean clone 重建并比对 build metadata 与依赖图。

## gate metadata（由证据提交写入；sbom-gate.sh 逐字段比对 series.yaml）

sbom_source_commit: 05289a36b7d2dc0db62fe1d8fa0bb94a4abc5f84
sbom_source_tree: 972b1ac3a5be0b3b9fd49b6f383feab990b5beab
sbom_cargo_lock_sha256: 781fd2792f915e5d60bbf1fd2b3d40e15b7e56b644d34b8b25d098bc9a707fa8
sbom_module_bazel_lock_sha256: be65892b2bdb12c0867dabfcaaf7ca4ee7a9ae4915e0d0de9c99448158777d31
sbom_status: generated-from-locks
sbom_artifact_status: external-receipt-required
