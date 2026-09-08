#!/usr/bin/env sh
set -eu

cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

case "${1:-write}" in
  write)
    prettier_mode=--write
    check_mode=
    ;;
  check | --check)
    prettier_mode=--check
    check_mode=--check
    ;;
  *)
    printf '%s\n' "usage: $0 [write|check]" >&2
    exit 2
    ;;
esac

# Check every formatter before any source is rewritten.
prettier_version=$(prettier --version 2>/dev/null) || {
  printf '%s\n' "format: Prettier 3.6.2 is required" >&2
  exit 1
}
if [ "$prettier_version" != "3.6.2" ]; then
  printf '%s\n' "format: Prettier 3.6.2 is required; found $prettier_version" >&2
  exit 1
fi
cargo +nightly-2026-07-03 fmt --version >/dev/null
# Ruff validates the pinned version and configuration without formatting files.
ruff check --no-cache --config ruff.toml --show-settings scripts/check-markdown-links.py >/dev/null

prettier "$prettier_mode" --no-config --ignore-path .gitignore --print-width 100 \
  --prose-wrap always --tab-width 2 '**/*.md'
cargo +nightly-2026-07-03 fmt --all -- --config-path rustfmt-nightly.toml ${check_mode:+"$check_mode"}
if [ -f fuzz/Cargo.toml ]; then
  cargo +nightly-2026-07-03 fmt --manifest-path fuzz/Cargo.toml -- \
    --config-path rustfmt-nightly.toml ${check_mode:+"$check_mode"}
fi
ruff format --no-cache --config ruff.toml ${check_mode:+"$check_mode"} .
