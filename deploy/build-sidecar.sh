#!/usr/bin/env bash
# 唯一 sidecar 镜像构建入口（R10-F6/F7 重构）：显式子命令 + 独立在线
# prepare / 离线 build。
#
#   BUILD_PROFILE=local deploy/build-sidecar.sh prepare
#   BUILD_PROFILE=local deploy/build-sidecar.sh build --mode local-cargo
#   deploy/build-sidecar.sh prepare            # release/linux/amd64（默认）
#   deploy/build-sidecar.sh build --mode <m>   # 离线（local-cargo 强制
#                                              #   --network none）：只消费
#                                              #   预置归档，零网络构建
#
# identity 模型（R9-F3 / R10）:
#   source_commit            = release: manifest；local: clean invocation HEAD
#   build_invocation_commit  = 构建调用时主仓 HEAD（receipt 记录）
#   build_mode               = local-cargo（发布批准）| buildx-experimental
#   release_tag              = manifest release_tag（指向 release_commit）
#
# 环境变量: BUILD_PROFILE(release|local；默认 release) PLATFORM IMAGE_TAG
#           RELEASE_ID
#           CTD_V8_CACHE(默认 ~/.cache/codex-transportd/v8)
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
V8_CACHE="${CTD_V8_CACHE:-$HOME/.cache/codex-transportd/v8}"
# shellcheck source=deploy/build-sidecar-profile-lib.sh
source "$REPO/deploy/build-sidecar-profile-lib.sh"

# ---------- smoke volume ownership（R18-F2/FXF-0001） ----------
# 生产函数在 deploy/smoke-volume-lib.sh（单一事实源）；self-test 独立脚本
# deploy/build-sidecar-volume-test.sh 驱动真实 build 路径。
# shellcheck source=deploy/smoke-volume-lib.sh
source "$(cd "$(dirname "$0")/.." && pwd)/deploy/smoke-volume-lib.sh"

# ---------- self-test-volume（R18-F2/FXF-0001 回归入口） ----------
# 委托给独立测试脚本（测试矩阵 + mock 编排不在生产入口内）。
if [ "${1:-}" = "self-test-volume" ]; then
  [ "$#" -le 1 ] || { echo "FAIL: self-test-volume 不接受多余参数"; exit 1; }
  exec bash "$(cd "$(dirname "$0")" && pwd)/build-sidecar-volume-test.sh" "$@"
fi
if [ "${1:-}" = "self-test-volume-cache-probe" ]; then
  # FRW12 兼容委托（M4）：probe 私有模式迁移至测试脚本，保留兼容期。
  exec bash "$(cd "$(dirname "$0")" && pwd)/build-sidecar-volume-test.sh" "$@"
fi

ctd_configure_build_profile "$REPO"
ctd_assert_native_build_daemon
V8_NAME="librusty_v8_release_${TRIPLE}.a.gz"
V8_EXPECTED_SHA="$(sed -n "s/^v8_archive_sha256_${TRIPLE}: //p" "$REPO/patches/transportd/series.yaml" | tr -d ' ')"
[ -n "$V8_EXPECTED_SHA" ] || { echo "FAIL: series.yaml 缺少 v8_archive_sha256_${TRIPLE}"; exit 1; }

# ---------- prepare（在线，唯一联网步骤） ----------
if [ "${1:-}" = "prepare" ]; then
  [ "$#" -le 1 ] || { echo "FAIL: prepare 不接受多余参数"; exit 1; }
  echo "== prepare（profile=${BUILD_PROFILE} platform=${PLATFORM}）：镜像 + v8 + Cargo closure =="
  # R11-F4：build 必须完全离线——prepare 预拉全部基础镜像（build 阶段
  # docker pull 会被禁止）。
  echo "   pull ${RUST_IMAGE}"
  docker pull --platform "$PLATFORM" "$RUST_IMAGE" >/dev/null
  echo "   pull ${DEBIAN_IMAGE}"
  docker pull --platform "$PLATFORM" "$DEBIAN_IMAGE" >/dev/null
  # v8 archive（固定 SHA256）
  mkdir -p "$V8_CACHE"
  V8_URL="https://github.com/denoland/rusty_v8/releases/download/v149.2.0/${V8_NAME}"
  V8_ARCHIVE="$V8_CACHE/${V8_NAME}"
  V8_CACHE_SHA=""
  if [ -f "$V8_ARCHIVE" ]; then
    V8_CACHE_SHA="$(shasum -a 256 "$V8_ARCHIVE" | awk '{print $1}')"
  fi
  if [ "$V8_CACHE_SHA" = "$V8_EXPECTED_SHA" ]; then
    echo "   reuse ${V8_NAME}（SHA256=${V8_EXPECTED_SHA}）"
  else
    echo "   下载 ${V8_NAME}（SHA256=${V8_EXPECTED_SHA}）"
    V8_TMP="$V8_ARCHIVE.tmp"
    curl -fsSL --retry 3 --max-time 300 "$V8_URL" -o "$V8_TMP"
    ACTUAL="$(shasum -a 256 "$V8_TMP" | awk '{print $1}')"
    [ "$ACTUAL" = "$V8_EXPECTED_SHA" ] || {
      rm -f "$V8_TMP"
      echo "FAIL: v8 SHA256 不匹配（期望 ${V8_EXPECTED_SHA} 实际 ${ACTUAL}）"
      exit 1
    }
    mv "$V8_TMP" "$V8_ARCHIVE"
  fi
  echo "   cached: $V8_CACHE/${V8_NAME} ($(wc -c < "$V8_CACHE/${V8_NAME}" | tr -d ' ') bytes) ✓"
  # R12-F3：Cargo closure 必须来自 source_commit 的不可变 clone（与 build
  # 同一 source——prepare 对当前仓库 fetch 会混入未发布变更）。
  SRC_COMMIT="$(ctd_resolve_source_commit "$REPO")"
  if [ "$(printf '%s' "$SRC_COMMIT" | wc -c | tr -d ' ')" -ne 40 ]; then
    echo "FAIL: source_commit 缺失/非法（manifest）"; exit 1
  fi
  PREP_SRC="$(mktemp -d "${TMPDIR:-/tmp}/ctd-prep.XXXXXX")"  # A06-T001：可移植 X 模板（GNU mktemp 拒绝无 X 模板）
  trap 'rm -rf "$PREP_SRC"' EXIT
  git clone --quiet "$REPO" "$PREP_SRC"
  git -C "$PREP_SRC" checkout --quiet "$SRC_COMMIT"
  echo "   cargo fetch --locked（source=${SRC_COMMIT}）"
  docker run --rm --pull=never --platform "$PLATFORM" \
    -v "$PREP_SRC":/src:ro \
    -v "$HOME/.cargo/git":/usr/local/cargo/git \
    -v "$HOME/.cargo/registry":/usr/local/cargo/registry \
    "$RUST_IMAGE" \
    bash -c "export PATH=\"/usr/local/rustup/toolchains/1.95.0-${TRIPLE}/bin:\$PATH\"; cd /src/codex-rs && cargo fetch --locked"
  rm -rf "$PREP_SRC"
  trap - EXIT
  echo "prepare 完成（build 可完全离线）"
  exit 0
fi

# ---------- build（离线；--network none） ----------
if [ "${1:-}" = "build" ]; then
  shift
  MODE=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --mode) MODE="$2"; shift 2 ;;
      # R11-F9：无 --platform 参数——PLATFORM 由 env 唯一。
      *) echo "FAIL: 未知 build 参数: $1"; exit 1 ;;
    esac
  done
  case "$MODE" in
    local-cargo) : ;;
    buildx-experimental) : ;;
    *) echo "FAIL: build --mode 必须为 local-cargo 或 buildx-experimental（实际: '$MODE'）"; exit 1 ;;
  esac
  if [ "$BUILD_PROFILE" = "local" ] && [ "$MODE" != "local-cargo" ]; then
    echo "FAIL: local profile only supports local-cargo mode"
    exit 1
  fi
  # R11-F4：build 必须完全离线——基础镜像必须已本地存在（prepare 预拉），
  # docker build/run 不再触发任何 pull。
  docker image inspect "$RUST_IMAGE" >/dev/null 2>&1 || {
    echo "FAIL: rust 基础镜像未预置（先运行: deploy/build-sidecar.sh prepare）"; exit 1; }
  docker image inspect "$DEBIAN_IMAGE" >/dev/null 2>&1 || {
    echo "FAIL: debian 基础镜像未预置（先运行: deploy/build-sidecar.sh prepare）"; exit 1; }
  # R10-F7：build 命令必须消费预置归档——未 prepare 直接 FAIL（非仅阶段命名）。
  [ -f "$V8_CACHE/${V8_NAME}" ] || {
    echo "FAIL: v8 archive 未预置（先运行: deploy/build-sidecar.sh prepare）"; exit 1; }
  ACTUAL="$(shasum -a 256 "$V8_CACHE/${V8_NAME}" | awk '{print $1}')"
  [ "$ACTUAL" = "$V8_EXPECTED_SHA" ] || { echo "FAIL: 预置 v8 SHA256 漂移"; exit 1; }
else
  echo "FAIL: 用法 build-sidecar.sh {self-test-volume|prepare|build --mode <local-cargo|buildx-experimental>}"
  exit 1
fi

# ---------- 共享构建流程 ----------
BUILD_STARTED_EPOCH="$(date +%s)"
RUN_ID="$(date +%s)-$$"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ctd-build.XXXXXX")"   # 脚本独占临时目录（R10-F10，可移植 X 模板）
CTX=""
CID=""
TARGET_OWNED=0
TARGET_DIR="${CARGO_TARGET_CACHE:-$DEFAULT_TARGET_CACHE}"
if [ -z "$TARGET_DIR" ]; then
  TARGET_DIR="$WORK_DIR/target"
  TARGET_OWNED=1
fi
mkdir -p "$TARGET_DIR"
ELF_CHECK_NAME="ctd-elf-check-${RUN_ID}"
# R17-F2/R18-F2：ownership 标记——资源仅在脚本显式创建成功后才置 owned=1;
# cleanup 只删除 owned 资源，绝不触碰外部/预建同名资源。
# （SMOKE_VOL/SMOKE_VOL_OWNED 初始化与 acquire/release 在文件顶部，单一事实源。）
ELF_CHECK_OWNED=0

# cleanup/trap/callsite 均为顶部共享函数（与 self-test 同一事实源）：
# cleanup（顶部定义，含 release_smoke_volume 委托）+ trap cleanup EXIT +
# smoke_volume_callsite（setup + fail closed）。
trap cleanup EXIT

# R18-F2：smoke volume 由 daemon 生成匿名名（消除可预测名 TOCTOU）；
# trap 已安装，任何后续失败都走 cleanup 删除。
if ! smoke_volume_callsite; then
  exit 1
fi

echo "== 0. 起始状态断言：工作区干净 + identity =="
DIRTY="$(git status --porcelain)"
[ -z "$DIRTY" ] || { echo "FAIL: 起始工作区不干净:"; echo "$DIRTY" | head -5; exit 1; }
HEAD0="$(git rev-parse HEAD)"
TREE0="$(git rev-parse 'HEAD^{tree}')"
SOURCE_COMMIT="$(ctd_resolve_source_commit "$REPO")"
if [ "$(printf '%s' "$SOURCE_COMMIT" | wc -c | tr -d ' ')" -ne 40 ]; then
  echo "FAIL: source_commit 必须是完整 40 位 commit: '$SOURCE_COMMIT'"; exit 1
fi
echo "   source_commit=${SOURCE_COMMIT}（kind=${SOURCE_ID_KIND}）"
echo "   build_invocation_commit=$HEAD0"
echo "   build_profile=$BUILD_PROFILE platform=$PLATFORM"
echo "   build_mode=$MODE"

echo "== 1. 不可变构建源：clone + checkout source_commit =="
SRC="$WORK_DIR/src"
git clone --quiet "$REPO" "$SRC"
git -C "$SRC" checkout --quiet "$SOURCE_COMMIT"
[ "$(git -C "$SRC" rev-parse HEAD)" = "$SOURCE_COMMIT" ] || { echo "FAIL: clone checkout 不一致"; exit 1; }
SOURCE_TREE="$(git -C "$SRC" rev-parse 'HEAD^{tree}')"
LOCK0="$(git -C "$SRC" show "HEAD:codex-rs/Cargo.lock" | shasum -a 256 | awk '{print $1}')"
echo "   source=$SOURCE_COMMIT lock=$LOCK0"

echo "== 2. 构建（--locked，${MODE}） =="
if [ "$MODE" = "local-cargo" ]; then
  echo "   [local-cargo] --network none + 只读 source + 独立 target"
  docker run --rm --pull=never --network none --platform "$PLATFORM" \
    -v "$SRC":/src:ro \
    -v "$TARGET_DIR":/target \
    -v "$V8_CACHE":/v8-cache:ro \
    -v "$HOME/.cargo/git":/usr/local/cargo/git:ro \
    -v "$HOME/.cargo/registry":/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/target \
    -e CODEX_TRANSPORTD_RELEASE_ID="$RELEASE_ID" \
    -e CTD_EXPECTED_UNAME="$EXPECTED_UNAME" \
    -e "RUSTY_V8_ARCHIVE=/v8-cache/${V8_NAME}" \
    "$RUST_IMAGE" \
    bash -c "actual=\$(uname -m); [ \"\$actual\" = \"\$CTD_EXPECTED_UNAME\" ] || { echo \"FAIL: builder uname=\$actual expected=\$CTD_EXPECTED_UNAME\"; exit 1; }; export PATH=\"/usr/local/rustup/toolchains/1.95.0-${TRIPLE}/bin:\$PATH\"; cd /src/codex-rs && cargo build --release --locked --offline -p codex-transportd"
  ELF="$TARGET_DIR/release/codex-transportd"
  [ -f "$ELF" ] || { echo "FAIL: ELF 未产出"; exit 1; }
  CTX="$WORK_DIR/ctx"
  mkdir -p "$CTX"
  cp "$ELF" "$CTX/codex-transportd"
  VENDOR_DEBS_DIR="$SRC/$VENDOR_DEBS_REL"
  [ -f "$VENDOR_DEBS_DIR/manifest.sha256" ] || {
    echo "FAIL: $PLATFORM vendor deb manifest missing: $VENDOR_DEBS_DIR/manifest.sha256"
    exit 1
  }
  if find "$VENDOR_DEBS_DIR" -name '*.deb' ! -name "*_${DEB_ARCH}.deb" | grep -q .; then
    echo "FAIL: $VENDOR_DEBS_DIR contains a foreign-architecture deb"
    exit 1
  fi
  cp -r "$VENDOR_DEBS_DIR" "$CTX/vendor-debs"
  # R13-F3：runtime 构建完全离线——--network none（RUN/deb maintainer
  # script 无法联网）+ --pull=false（镜像必须已由 prepare 预拉，禁止
  # 触发任何 pull）。
  docker build --network none --pull=false --platform "$PLATFORM" \
    -f "$SRC/codex-rs/codex-transportd/Dockerfile.runtime" \
    -t "$IMAGE_TAG" "$CTX" >/dev/null
else
  echo "   [buildx-experimental] 未签收路径（R9-F6）——仅实验"
  docker buildx build --network none --pull=false --platform "$PLATFORM" --load \
    --build-arg "RELEASE_ID=$RELEASE_ID" \
    -f "$SRC/codex-rs/codex-transportd/Dockerfile" -t "$IMAGE_TAG" "$SRC" >/dev/null
fi

echo "== 3. 构建后状态断言 =="
[ "$(git rev-parse HEAD)" = "$HEAD0" ] || { echo "FAIL: 构建期间主仓 HEAD 变化"; exit 1; }
[ "$(git rev-parse 'HEAD^{tree}')" = "$TREE0" ] || { echo "FAIL: 构建期间主仓 tree 变化"; exit 1; }
DIRTY2="$(git status --porcelain)"
[ -z "$DIRTY2" ] || { echo "FAIL: 构建后主仓工作区不干净:"; echo "$DIRTY2" | head -5; exit 1; }
echo "   HEAD/tree/status 不变 ✓"

echo "== 4. 镜像/ELF 校验 =="
ARCH="$(docker image inspect "$IMAGE_TAG" --format '{{.Architecture}}')"
[ "$ARCH" = "${PLATFORM#linux/}" ] || { echo "FAIL: 镜像架构 $ARCH != ${PLATFORM#linux/}"; exit 1; }
echo "   Architecture=$ARCH ✓"
if [ "$MODE" = "local-cargo" ]; then
  ELF_CHECK="$ELF"
else
  # R17-F2：create 成功后才置 owned——cleanup 只删本轮创建的容器。
  if docker create --pull=never --name "$ELF_CHECK_NAME" --platform "$PLATFORM" "$IMAGE_TAG" >/dev/null 2>&1; then
    ELF_CHECK_OWNED=1
    docker cp "$ELF_CHECK_NAME":/app/codex-transportd "$WORK_DIR/elf" 2>/dev/null || true
  fi
  ELF_CHECK="$WORK_DIR/elf"
fi
ELF_INFO="$(file "$ELF_CHECK")"
echo "$ELF_INFO" | grep -q "ELF" || { echo "FAIL: 产物非 ELF"; exit 1; }
case "$TRIPLE:$ELF_INFO" in
  x86_64-unknown-linux-gnu:*x86-64*) : ;;
  aarch64-unknown-linux-gnu:*ARM\ aarch64*) : ;;
  *) echo "FAIL: ELF architecture does not match $TRIPLE: $ELF_INFO"; exit 1 ;;
esac
echo "   ELF target=$TRIPLE ✓"

echo "== 5. /info 完整 pin 校验 =="
# R12-F2：digest 统一为裸 OCI 格式 sha256:hex（与 Compose CODEX_TRANSPORTD_DIGEST 同口径）。
if [ "$BUILD_PROFILE" = "local" ]; then
  DIGEST="$(docker image inspect "$IMAGE_TAG" --format '{{.Id}}')"
else
  DIGEST="$(docker image inspect "$IMAGE_TAG" --format '{{index .RepoDigests 0}}' | sed 's/.*@//')"
fi
# R17-F2/R18-F2：smoke volume 已在共享段创建（daemon 匿名名，owned=1），
# 此处直接复用，不再重复 create（避免幂等 create 掩盖 ownership 语义）。
docker run --rm --pull=never --platform "$PLATFORM" --user 0:0 -v "$SMOKE_VOL":/run/ctd -v "$SMOKE_VOL":/var/lib/ctd-home \
  --entrypoint /bin/sh "$IMAGE_TAG" -ec "install -d -m 0770 -o 10001 -g 2000 /run/ctd/sock; install -d -m 0750 -o 10001 -g 2000 /var/lib/ctd-home/home" >/dev/null
CID="$(docker run -d --pull=never --platform "$PLATFORM" \
  -v "$SMOKE_VOL":/run/ctd -v "$SMOKE_VOL":/var/lib/ctd-home \
  "$IMAGE_TAG" --socket /run/ctd/sock/transport.sock --codex-home /var/lib/ctd-home/home \
  --socket-dir-uid 10001 --socket-dir-gid 2000 \
  --image-digest "$DIGEST")"
INFO_TMP="$WORK_DIR/info"
for i in $(seq 1 30); do
  docker exec "$CID" curl -fsS --unix-socket /run/ctd/sock/transport.sock http://localhost/_internal/v1/info \
    >"$INFO_TMP.json" 2>/dev/null && break
  sleep 1
  [ "$i" = 30 ] && { echo "FAIL: sidecar /info 不可达"; docker logs "$CID" 2>&1 | tail -3; exit 1; }
done
python3 - "$SOURCE_COMMIT" "$TRIPLE" "$LOCK0" "$RELEASE_ID" "$DIGEST" "$BUILD_PROFILE" "$INFO_TMP.json" <<'PYEOF'
import json, sys
expect_head, expect_triple, expect_lock, expect_release, expect_digest, profile, path = sys.argv[1:]
info = json.load(open(path))
checks = {
    'fork_commit': (info.get('fork_commit'), expect_head),
    'target': (info.get('target'), expect_triple),
    'fork_cargo_lock_sha256': (info.get('fork_cargo_lock_sha256'), expect_lock),
    'release_id': (info.get('release_id'), expect_release),
    'image_digest': (info.get('image_digest'), expect_digest),
}
failed = False
for k, (actual, want) in checks.items():
    if actual != want:
        print(f'FAIL: /info {k}={actual} != {want}')
        failed = True
if info.get('attestation') is not False:
    print(f'FAIL: attestation={info.get("attestation")}（生产必须 false）')
    failed = True
if info.get('test_upstream_enabled') is not False:
    print(f'FAIL: test_upstream_enabled={info.get("test_upstream_enabled")}（生产必须 false）')
    failed = True
is_dev = info.get('release_id', '').endswith('-dev')
if (profile == 'release' and is_dev) or (profile == 'local' and not is_dev):
    print(f'FAIL: release_id={info.get("release_id")} 与 profile={profile} 不一致')
    failed = True
if failed:
    sys.exit(1)
print('   /info pin 全部一致 ✓ (commit/target/lock/release_id/image_digest/attestation/test_upstream)')
PYEOF
docker exec "$CID" curl -fsS --unix-socket /run/ctd/sock/transport.sock http://localhost/_internal/v1/health/ready >/dev/null \
  || { echo "FAIL: health/ready"; exit 1; }
echo "   health/ready ✓（非 root 10001:2000 运行）"

echo "== 6. digest receipt（含 build_mode） =="
RECEIPT="$RECEIPT_PATH"
mkdir -p "$(dirname "$RECEIPT")"
BUILD_ELAPSED_SECONDS="$(( $(date +%s) - BUILD_STARTED_EPOCH ))"
cat > "$RECEIPT" <<EOF
image:                    $IMAGE_TAG
build_profile:            $BUILD_PROFILE
platform:                 $PLATFORM
arch:                     $ARCH
triple:                   $TRIPLE
deb_arch:                 $DEB_ARCH
digest:                   $DIGEST
release_id:               $RELEASE_ID
source_commit:            $SOURCE_COMMIT
source_tree:              $SOURCE_TREE
build_invocation_commit:  $HEAD0
build_mode:               $MODE
lock_sha256:              $LOCK0
v8_sha256:                $V8_EXPECTED_SHA
elapsed_seconds:           $BUILD_ELAPSED_SECONDS
build:                    $(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
cat "$RECEIPT"
echo "build-sidecar.sh 全部通过（profile=${BUILD_PROFILE} mode=${MODE} receipt=${RECEIPT}）"
