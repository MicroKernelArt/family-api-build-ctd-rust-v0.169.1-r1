#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="$REPO/replay/manifest.yaml"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

field() {
  sed -n "s/^$1: //p" "$MANIFEST"
}

cd "$REPO"
LINE="$(field line)"
BASE="$(field upstream_commit)"
SOURCE_COMMIT="$(field source_commit)"
SOURCE_TREE="$(field source_tree)"
EXPECTED_COUNT="$(field compact_count)"
COMMITS="$(sed -n 's/^  - commit: //p' "$MANIFEST")"
TREES="$(sed -n 's/^    tree: //p' "$MANIFEST")"

CURRENT_BRANCH="$(git symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
if [ "$CURRENT_BRANCH" != "$LINE" ]; then
  [ "${GITHUB_ACTIONS:-}" = "true" ] || fail "branch is not $LINE"
  [ "${GITHUB_SHA:-}" = "$(git rev-parse HEAD)" ] || fail "GitHub SHA is not HEAD"
  CI_PREFIX="ci/family-api-codex/"
  case "${GITHUB_REF_NAME:-}" in
    "$CI_PREFIX"*) ;;
    *) fail "GitHub ref is outside $CI_PREFIX" ;;
  esac
  CI_SUFFIX="${GITHUB_REF_NAME#"$CI_PREFIX"}"
  CI_TASK="${CI_SUFFIX%/*}"
  CI_SHA="${CI_SUFFIX##*/}"
  [ -n "$CI_TASK" ] && [ "$CI_TASK" = "${CI_TASK##*/}" ] || fail "CI task ref is malformed"
  [ "$CI_SHA" = "$GITHUB_SHA" ] || fail "CI ref SHA is not HEAD"
fi
git cat-file -e "$BASE^{commit}" 2>/dev/null || fail "upstream base is unavailable"

TREE_ARGS=()
while IFS= read -r tree; do
  [ -n "$tree" ] && TREE_ARGS+=("$tree")
done <<< "$TREES"
set -- "${TREE_ARGS[@]}"
PREV="$BASE"
COUNT=0
for COMMIT in $COMMITS; do
  [ "$#" -gt 0 ] || fail "tree list shorter than commit list"
  EXPECTED_TREE="$1"
  shift
  git cat-file -e "$COMMIT^{commit}" 2>/dev/null || fail "compact commit unavailable: $COMMIT"
  [ "$(git rev-parse "$COMMIT^")" = "$PREV" ] || fail "non-linear compact commit: $COMMIT"
  [ "$(git rev-parse "$COMMIT^{tree}")" = "$EXPECTED_TREE" ] || fail "tree mismatch: $COMMIT"
  PREV="$COMMIT"
  COUNT=$((COUNT + 1))
done
[ "$#" -eq 0 ] || fail "tree list longer than commit list"
[ "$COUNT" -eq "$EXPECTED_COUNT" ] || fail "compact count $COUNT != $EXPECTED_COUNT"
[ "$(git rev-parse "$PREV^{tree}")" = "$SOURCE_TREE" ] || fail "final capability tree mismatch"

REPLAY_HEAD="$(git rev-parse HEAD)"
if [ "$CURRENT_BRANCH" != "$LINE" ]; then
  while [ "$REPLAY_HEAD" != "$BASE" ]; do
    REPLAY_PARENT="$(git rev-parse "$REPLAY_HEAD^" 2>/dev/null)" || fail "CI candidate parent is unavailable"
    [ "$REPLAY_PARENT" = "$PREV" ] && break
    REPLAY_HEAD="$REPLAY_PARENT"
  done
fi
[ "$(git rev-parse "$REPLAY_HEAD^")" = "$PREV" ] || fail "replay metadata commit is not on the compact line"
[ "$(git rev-list --count "$BASE..$REPLAY_HEAD")" -eq $((EXPECTED_COUNT + 1)) ] || fail "unexpected commits in replay line"
for PATHNAME in $(git diff-tree --no-commit-id --name-only -r "$REPLAY_HEAD"); do
  case "$PATHNAME" in
    replay/*) ;;
    *) fail "replay metadata commit changed non-replay path: $PATHNAME" ;;
  esac
done

if git cat-file -e "$SOURCE_COMMIT^{commit}" 2>/dev/null; then
  [ "$(git rev-parse "$SOURCE_COMMIT^{tree}")" = "$SOURCE_TREE" ] || fail "frozen source tree mismatch"
  echo "source object verified: $SOURCE_COMMIT"
else
  echo "source object unavailable; source tree remains pinned: $SOURCE_TREE"
fi

[ -z "$(git status --porcelain)" ] || fail "worktree is not clean"
echo "PASS: $LINE $EXPECTED_COUNT compact commits, source tree $SOURCE_TREE"
