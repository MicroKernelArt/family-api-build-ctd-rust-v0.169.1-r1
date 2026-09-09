# UPSTREAM_BASE

The replay0905 source line is based on the immutable OpenAI Codex
`rust-v0.159.0` release object.

| Field | Value |
|---|---|
| upstream repository | `https://github.com/openai/codex` |
| annotated tag | `rust-v0.159.0` |
| tag object | `377f7f557a6bdea0f3a2d26d4d899c66db4789d0` |
| peeled base commit | `687a119f0fcaace47e1f1abcc77cec6c813fd6da` |
| base tree | `bee1375c8502e4c4d71db017f7e087dfbb3e775c` |
| base `codex-rs/Cargo.lock` SHA-256 | `16b915e94730a7e598182620923fc6eae19e32366d3b5ef91128002575ae24af` |
| base `MODULE.bazel.lock` SHA-256 | `be65892b2bdb12c0867dabfcaaf7ca4ee7a9ae4915e0d0de9c99448158777d31` |
| Rust toolchain | `1.95.0` |
| license | Apache-2.0 |

Verification uses the objects already shipped by this repository:

```bash
git cat-file -p 377f7f557a6bdea0f3a2d26d4d899c66db4789d0
git rev-parse '377f7f557a6bdea0f3a2d26d4d899c66db4789d0^{}'
git show 687a119f0fcaace47e1f1abcc77cec6c813fd6da:codex-rs/Cargo.lock | shasum -a 256
git show 687a119f0fcaace47e1f1abcc77cec6c813fd6da:MODULE.bazel.lock | shasum -a 256
```

Changing any base identity requires regenerating the complete patch series and
all source-bound evidence.
