# UPSTREAM_BASE

The replay0905 source line is based on the immutable OpenAI Codex
`rust-v0.159.2` release object.

| Field | Value |
|---|---|
| upstream repository | `https://github.com/openai/codex` |
| annotated tag | `rust-v0.159.2` |
| tag object | `8b9fa496bbf2c47aebd62e85a080b9a522a455b5` |
| peeled base commit | `ff6aec96948b70d94983af2641a6b67c94faeff5` |
| base tree | `406dfdd5c68f303a3a8d04f32b3965b3b0ca0361` |
| base `codex-rs/Cargo.lock` SHA-256 | `4fab6fe99180e2b8f305bd4b33c1b4bd825e3fa4cdef1045be750843bd3d431e` |
| base `MODULE.bazel.lock` SHA-256 | `be65892b2bdb12c0867dabfcaaf7ca4ee7a9ae4915e0d0de9c99448158777d31` |
| Rust toolchain | `1.95.0` |
| license | Apache-2.0 |

Previous base in this line: `rust-v0.159.0` (`687a119f0fcaace47e1f1abcc77cec6c813fd6da`,
tree `bee1375c8502e4c4d71db017f7e087dfbb3e775c`, lock
`16b915e94730a7e598182620923fc6eae19e32366d3b5ef91128002575ae24af`). It was
superseded because the Go-side sub2api implementation tracks `rust-v0.159.2`.
`MODULE.bazel.lock` and the `v8` pin (`v8 150.4.0`) are byte-identical across
both bases.

Verification uses the objects already shipped by this repository:

```bash
git cat-file -p 8b9fa496bbf2c47aebd62e85a080b9a522a455b5
git rev-parse '8b9fa496bbf2c47aebd62e85a080b9a522a455b5^{}'
git show ff6aec96948b70d94983af2641a6b67c94faeff5:codex-rs/Cargo.lock | shasum -a 256
git show ff6aec96948b70d94983af2641a6b67c94faeff5:MODULE.bazel.lock | shasum -a 256
```

Changing any base identity requires regenerating the complete patch series and
all source-bound evidence.
