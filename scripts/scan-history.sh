#!/usr/bin/env bash
set -euo pipefail

flags=(--redact --no-banner --validation=false --ignore-gitleaks-allow)
betterleaks git . --log-opts="--all --reflog --full-history" "${flags[@]}"
git log --all --reflog --format=%B | betterleaks stdin "${flags[@]}"
git for-each-ref --format='%(contents)' refs/tags | betterleaks stdin "${flags[@]}"
