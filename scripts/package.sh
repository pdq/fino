#!/usr/bin/env bash
set -euo pipefail

target="${1:?usage: package.sh TARGET}"
case "$target" in
  aarch64-apple-darwin|aarch64-unknown-linux-musl|x86_64-unknown-linux-musl) ;;
  *) echo "unsupported release target: $target" >&2; exit 2 ;;
esac

package_id=$(cargo pkgid --locked)
version=${package_id##*@}
if [[ -n "${RELEASE_TAG:-}" && "$RELEASE_TAG" != "v$version" ]]; then
  echo "release tag does not match the package version" >&2
  exit 2
fi

stage="target/package/$target"
mkdir -p "$stage" dist
cp "target/$target/release/fino" "$stage/fino"
cp README.md "$stage/README.md"
cp THIRD_PARTY_NOTICES "$stage/THIRD_PARTY_NOTICES"
cp vendor/tree-sitter/LICENSE "$stage/tree-sitter-LICENSE"
cp vendor/tree-sitter-elixir/LICENSE "$stage/tree-sitter-elixir-LICENSE"
cp vendor/tree-sitter-elixir/PATCH.md "$stage/tree-sitter-elixir-PATCH.md"
cp vendor/tree-sitter/src/unicode/LICENSE "$stage/unicode-LICENSE"
COPYFILE_DISABLE=1 tar -czf "dist/fino-v$version-$target.tar.gz" -C "$stage" fino README.md THIRD_PARTY_NOTICES tree-sitter-LICENSE tree-sitter-elixir-LICENSE tree-sitter-elixir-PATCH.md unicode-LICENSE
