# Build and test Kapsel

For source development, start below. To install and try the published beta instead, use the
[evaluation guide](EVALUATOR.md). The service and installer in repository HEAD remain unpublished.
[Testing](TESTING.md) explains what each test proves; this page owns setup and commands.

## Prerequisites

Use a macOS or Linux development host with Git and a C compiler/linker. Install
[Rust through rustup](https://rust-lang.org/tools/install/), which also supplies Cargo.
`rust-toolchain.toml` selects Rust 1.98.0, Clippy, and rustfmt for this checkout.

Run all commands below from the repository root. The first build downloads the pinned toolchain and
Cargo dependencies. Building the ordinary executable does not require Python, Node.js, or Docker.

## Evaluator CLI

Build and check the local executable:

```sh
cargo build --locked --bin kapsel
target/debug/kapsel --version
```

This builds repository HEAD, not the published v0.2.0 artifact. See [Commands](COMMANDS.md) for the
CLI's fixed forms and operator-owned inputs. For an end-to-end source demonstration, use the
[crash-recovery demo](#public-crash-recovery-demonstration).

## Deterministic gate and formatting

For contributor checks, also install [Python](https://www.python.org/downloads/) 3.11+ with `venv`
and [Node.js](https://nodejs.org/en/download) 24 with npm. Then install the pinned formatters:

```sh
rustup toolchain install nightly-2026-07-03 --profile minimal --component rustfmt
npm install --global prettier@3.6.2
python3 -m venv "$HOME/.local/share/kapsel/dev-tools"
. "$HOME/.local/share/kapsel/dev-tools/bin/activate"
python -m pip install ruff==0.16.6
```

The virtual environment keeps Ruff out of system Python and the worktree. Activate it again in each
new shell. An existing tool manager is equally fine if the same versions are on `PATH`.

The everyday loop is:

```sh
./scripts/format.sh
./scripts/ci-local.sh
```

Formatting runs **Markdown, Rust, then Python**, including the fuzz workspace and Python fixtures.
It checks tool availability before rewriting files and does not apply lint fixes. To check layout
without changing source, run `./scripts/format.sh --check`.

The local gate checks formatting, Python lint, Markdown links, tooling regressions, Rust line width,
Clippy, rustdoc, deterministic Rust tests, and doctests. It does not start Docker or a cluster. For
a smaller check:

```sh
./scripts/ci-local.sh static  # formatting, lint, links, and tooling regressions
./scripts/ci-local.sh rust    # Clippy, rustdoc, and deterministic Rust tests
./scripts/ci-local.sh doc     # Rust doctests
```

### Git hooks

Inspect any existing custom hook path before enabling the repository hooks:

```sh
git config --get core.hooksPath
git config core.hooksPath .githooks
```

Pre-commit checks formatting, Python lint, Rust width, workspace Clippy, and portable installer
tests. It requires no unstaged or untracked files and works offline after tools and Cargo
dependencies are installed. Pre-push requires a clean checkout matching the pushed tree and runs the
complete local gate, reusing a previous passing result for the same tree. Neither hook starts
Docker. See [the hooks](../.githooks/) for exact refusal and caching behavior.

## Focused gates

Choose the smallest check that owns the changed behavior. Run the complete local gate before handoff
when practical. Additional environment requirements are listed in the sections below.

| Change                             | Command                                                  |
| ---------------------------------- | -------------------------------------------------------- |
| Python scripts                     | `ruff check --no-cache --config ruff.toml .`             |
| Formatting pipeline                | `python3 scripts/test-format.py`                         |
| Effect gateway                     | `cargo test --locked -p kapsel`                          |
| Service and private harness        | `cargo test --locked -p kapseld --features test-harness` |
| Shared operator authority          | `cargo test --locked -p kapsel-authority`                |
| Portable installer                 | `cargo test --locked -p kapsel-installer`                |
| Service installed assets           | `cargo test --locked -p kapseld --test install_assets`   |
| MCP adapter                        | `cargo test --locked --test e2e_mcp_adapter`             |
| Crash-demo harness, without Docker | `./scripts/test-demo-harness.sh`                         |
| Seeded lifecycle simulation        | `./scripts/test-simulation.sh`                           |
| Receipt-inspection fuzz smoke      | `./scripts/test-fuzz.sh`                                 |
| Live Kubernetes behavior           | `./scripts/test-kind-effect-gateway.sh`                  |
| Linux installer/bundle scenarios   | `python3 scripts/test-kapsel-installer-bundle.py`        |
| Debian 12 identity experiment      | `./scripts/test-debian12-installer-identities.sh`        |

## Live Kubernetes gate

Requires Docker, kind 0.32+, kubectl 1.30+, Python 3.11+, and OpenSSL:

```sh
./scripts/test-kind-effect-gateway.sh
```

The script creates and removes its own uniquely named cluster and exports failure logs. It records
the base revision and working-tree diff digest, and refuses untracked files that cannot be included
in that evidence. This lane is separate from deterministic CI. See
[Live Kubernetes and demonstration](TESTING.md#live-kubernetes-and-demonstration) for the gateway,
admission, and frozen JSON Patch comparison cases.

## Public crash-recovery demonstration

Requires Docker, kind 0.32+, kubectl 1.30+, and Python 3.11+:

```sh
./scripts/demo-kind-crash-recovery.sh
```

The source demo builds its Rust harness, refuses pre-existing kind clusters, and cleans up its owned
cluster and workspace. To run the published artifact without a Rust toolchain, follow the
[evaluation guide](EVALUATOR.md#fastest-path).

## Independent client experiment

With the pinned kubectl v1.33.9 build and Python 3.11+, run:

```sh
python3 scripts/test-independent-kubectl.py
```

The [kubectl failure corpus](INDEPENDENT_TOOL_CORPUS.md) uses a loopback fixture, not a cluster or
Kapsel runtime. It is separate from the default gate.

## Kapsel service candidate

The service remains unpublished. The focused package command above includes its private harness. On
Linux, run the process tests:

```sh
cargo test --locked -p kapseld --features test-harness --test linux_process
```

With `sg` installed, include the ignored distinct-effective-group case:

```sh
cargo test --locked -p kapseld --features test-harness --test linux_process \
  distinct_effective_gid_is_denied_before_frame_read -- --ignored --exact
```

See [Kapsel service](KAPSEL_SERVICE.md) for the current boundary and
[service testing](TESTING.md#kapsel-service) for evidence coverage.

## Kapsel installer skeleton

The installer remains partial and unpublished. Default builds stop at `bundle_unavailable`. Portable
tests run without Docker; the Linux bundle lane needs Docker with `linux/amd64` support and OpenSSL:

```sh
python3 scripts/test-kapsel-installer-bundle.py
```

The launcher stages test-only payloads and runs named Rust integration tests in a disposable
container. CI runs this lane separately after the default gate. It is not a supported installation
path. [Installer testing](TESTING.md#installer) owns the exact proof and unimplemented boundaries.

Build caches live under `~/.cache/kapsel/installer`, overridden by `KAPSEL_INSTALLER_CACHE_DIR`.
Their key binds the builder image, toolchain, target, and lockfile. Only build inputs and compiler
output are reused; test-host state is always fresh. The launcher allows 2,400 seconds. Prefer native
x86-64 Linux for this lane; cold compilation under ARM emulation can exceed that bound.

The separate Debian 12 identity experiment also requires network access to install `sudo` inside its
disposable container:

```sh
./scripts/test-debian12-installer-identities.sh
```

It checks native account-tool behavior, not an installed Kapsel system.

## Upgrade and rollback fixture gate

Repository HEAD uses journal format 4 and rejects older versions without migration. Run the
rejection proof:

```sh
cargo test --locked -p kapsel --lib \
  gateway::tests::v011_upgrade::older_journal_versions_are_rejected_without_touching_rows -- --exact
```

The historical `scripts/test-v011-upgrade-fixtures.py` generator applies only to the pre-format-4
published baseline, not current upgrade support. See [Upgrade and rollback](UPGRADE.md) before using
historical fixtures or retained journals.

## Robustness lanes

Fuzzing requires cargo-fuzz 0.13+ and the pinned nightly toolchain. Check the target or run a
bounded smoke test:

```sh
rustup run nightly-2026-07-03 cargo fuzz check --manifest-path fuzz/Cargo.toml inspect_receipt
./scripts/test-fuzz.sh
```

For a longer run:

```sh
KAPSEL_FUZZ_RUNS=1000000 KAPSEL_FUZZ_MAX_TIME=3600 ./scripts/test-fuzz.sh
```

Run the seeded lifecycle simulation with defaults, or supply a seed and workload for replay:

```sh
./scripts/test-simulation.sh
KAPSEL_SIMULATION_SEED=21182435914953528 \
KAPSEL_SIMULATION_CASES=10000 KAPSEL_SIMULATION_SHARDS=8 ./scripts/test-simulation.sh
```

Optional `KAPSEL_FUZZ_NOTIFY_URL` and `KAPSEL_SIMULATION_NOTIFY_URL` send completion summaries
through `curl` to a destination you control.

For unattended runs, inspect [the soak runner](../scripts/run-nightly-soak.sh) first. It updates the
checkout by default, so use a dedicated checkout or disable updates explicitly:

```sh
KAPSEL_SOAK_AUTO_UPDATE=0 ./scripts/run-nightly-soak.sh
```

## Candidate qualification

This combines the default, live, fuzz, measurement, and security lanes against one committed clean
candidate. It additionally requires cargo-audit 0.22.2, Trivy 0.72.0 with current databases, Docker
access to the pinned builder image, and the host Cargo registry. It is not a first-run check.

```sh
python3 scripts/run-beta-qualification.py --output /tmp/beta-qualification-baseline.json
python3 scripts/validate-beta-qualification-baseline.py /tmp/beta-qualification-baseline.json
```

Qualification is finite candidate evidence, not a production or support claim.

## MCP adapter

After building the local executable, start the fixed stdio process with operator-owned
configuration:

```sh
target/debug/kapsel mcp --operator-config /absolute/operator.json
```

See [MCP](MCP.md) for protocol details. The focused-gate table lists its black-box test.

## Release artifact

Requires a clean checkout, Python 3.11+, and Docker with `linux/amd64` support. The sole release
target is `x86_64-unknown-linux-gnu`. Assemble the archive and sidecars under `dist/`:

```sh
python3 scripts/assemble-release-artifact.py --output-directory dist
```

For the complete two-assembly proof, keep output outside the worktree:

```sh
a_dir=$(mktemp -d "${TMPDIR:-/tmp}/kapsel-release-a.XXXXXX")
archive_a=$(python3 scripts/assemble-release-artifact.py --output-directory "$a_dir")
python3 scripts/test-release-artifact.py --archive "$archive_a"
python3 scripts/test-release-reproducibility.py --reference-archive "$archive_a"
```

Remove `"$a_dir"` when its evidence is no longer needed. [Release artifacts](RELEASE.md) owns
layout, authentication, publication, and reproducibility requirements. The
[evaluation guide](EVALUATOR.md) owns downloading, authenticating, and running the published beta.

## Coverage

With cargo-llvm-cov 0.8.7, matching CI:

```sh
cargo llvm-cov --locked --workspace --codecov --output-path codecov.json
```

Coverage is informational and non-blocking, not correctness evidence.

## Toolchain ownership

Cargo manifests and `Cargo.lock` own Rust dependencies. `rust-toolchain.toml` selects the compiler;
`rustfmt.toml`, `rustfmt-nightly.toml`, `clippy.toml`, and `ruff.toml` own style settings.
[`scripts/format.sh`](../scripts/format.sh), [`scripts/ci-local.sh`](../scripts/ci-local.sh), and
[CI](../.github/workflows/ci.yml) own tool invocation. Keep this guide's setup pins aligned with
those files when upgrading tools.
