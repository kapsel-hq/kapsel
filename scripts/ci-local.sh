#!/usr/bin/env sh
set -eu

run_static_checks() {
  echo "==> Markdown, Rust, and Python format"
  ./scripts/format.sh --check

  printf '%s\n' "==> Python lint"
  ruff check --no-cache --config ruff.toml .

  printf '%s\n' "==> Formatting pipeline regressions"
  python3 scripts/test-format.py

  printf '%s\n' "==> Rust line width"
  ./scripts/check-rust-width.sh

  printf '%s\n' "==> Beta qualification regressions"
  ./scripts/test-beta-qualification.py

  printf '%s\n' "==> Markdown link checker regressions"
  ./scripts/test-check-markdown-links.py

  printf '%s\n' "==> Markdown links"
  ./scripts/check-markdown-links.py

}

run_rust_checks() {
  echo "==> clippy"
  cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

  echo "==> rustdoc"
  RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

  echo "==> deterministic Rust tests"
  cargo test --locked --workspace --lib --bins --tests
}

run_documentation_tests() {
  echo "==> documentation tests"
  cargo test --locked --doc --workspace
}

case "${1:-all}" in
  all)
    run_static_checks
    run_rust_checks
    run_documentation_tests
    echo "==> Kapsel default gate passed"
    ;;
  static)
    run_static_checks
    echo "==> Static checks passed"
    ;;
  rust)
    run_rust_checks
    echo "==> Rust checks and deterministic tests passed"
    ;;
  doc)
    run_documentation_tests
    echo "==> Documentation tests passed"
    ;;
  *)
    printf '%s\n' "usage: $0 [all|static|rust|doc]" >&2
    exit 2
    ;;
esac
