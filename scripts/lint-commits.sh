#!/usr/bin/env bash
set -euo pipefail

git rev-list --all | while IFS= read -r commit; do
  git show -s --format=%B "$commit" |
    committed --config committed.toml --no-merge-commit --no-wip --no-fixup --commit-file -
done
