#!/usr/bin/env bash
set -euo pipefail

cluster_name="kapsel-demo-$$-${RANDOM}"
node_image="kindest/node:v1.33.12@sha256:3f5c8443c620245e4d355cfe09e96a91ead32ceaa569d3f1ca9edf0cb2fe2ff4"
fixture_image="registry.k8s.io/pause:3.10.1"
target_image="registry.k8s.io/pause@sha256:278fb9dbcca9518083ad1e11276933a2e96f23de604a3a08cc3c80002767d24c"
failed_image="registry.example.invalid/kapsel/unhealthy@sha256:1111111111111111111111111111111111111111111111111111111111111111"
log_max=65536
workspace=""
log_directory=""
cluster_owned=0
active_child_pid=""
demo_executable=""
asset_directory=""
source_root=""
demo_start_seconds=$SECONDS

validate_artifact_inputs() {
  if [[ $demo_executable != /* || ! -f $demo_executable ||
    ! -x $demo_executable || -L $demo_executable ]]; then
    printf 'artifact demo executable is unsafe or unavailable; extract the complete release archive again\n' >&2
    return 1
  fi
  if [[ $asset_directory != /* || ! -d $asset_directory ||
    -L $asset_directory ]]; then
    printf 'artifact demo asset directory is unsafe or unavailable; run from the extracted release directory\n' >&2
    return 1
  fi
  if [[ ! -f $asset_directory/kap0038-trust.hex ||
    -L $asset_directory/kap0038-trust.hex ]]; then
    printf 'artifact demo trust vector is unsafe or unavailable; extract the complete release archive again\n' >&2
    return 1
  fi
}

configure_demo_inputs() {
  local script_directory
  local release_root
  script_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
  if [[ -z ${KAPSEL_DEMO_EXECUTABLE:-} && -z ${KAPSEL_DEMO_ASSET_DIRECTORY:-} ]]; then
    if [[ $(basename "$script_directory") == kapsel &&
      $(basename "$(dirname "$script_directory")") == share ]]; then
      release_root=$(cd "$script_directory/../.." && pwd -P)
      demo_executable="$release_root/libexec/kapsel-demo-harness"
      asset_directory=$script_directory
      validate_artifact_inputs
      return
    fi
    source_root=$(cd "$script_directory/.." && pwd -P)
    demo_executable="$source_root/target/debug/kapsel"
    asset_directory="$source_root/vectors"
    return
  fi
  if [[ -z ${KAPSEL_DEMO_EXECUTABLE:-} || -z ${KAPSEL_DEMO_ASSET_DIRECTORY:-} ]]; then
    printf 'artifact demo requires both executable and asset directory\n' >&2
    return 1
  fi
  demo_executable=$KAPSEL_DEMO_EXECUTABLE
  asset_directory=$KAPSEL_DEMO_ASSET_DIRECTORY
  validate_artifact_inputs
}

phase() {
  printf '[demo +%ss %s/9] %s\n' "$((SECONDS - demo_start_seconds))" "$1" "$2"
}

bounded_log() {
  local source=$1
  local destination=$2
  if [[ -f $source ]]; then
    tail -c "$log_max" "$source" >"$destination"
  fi
}

sha256_file() {
  python3 - "$1" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
}

delete_owned_cluster() {
  if kind delete cluster --name "$cluster_name"; then
    cluster_owned=0
    printf '[demo cleanup] owned kind cluster removed: %s\n' "$cluster_name"
    return 0
  fi
  printf 'could not delete owned kind cluster: %s\n' "$cluster_name" >&2
  printf 'retry only this owned cluster: kind delete cluster --name %s\n' "$cluster_name" >&2
  return 1
}

cleanup() {
  local status=$?
  local diagnostic
  local log
  local owned_workspace=$workspace
  trap - EXIT INT TERM
  if [[ -n $active_child_pid ]] && kill -0 "$active_child_pid" 2>/dev/null; then
    kill -KILL "$active_child_pid" 2>/dev/null || true
    wait "$active_child_pid" 2>/dev/null || true
  fi
  active_child_pid=""
  if [[ $status -ne 0 && -n $workspace ]]; then
    log_directory=$(mktemp -d "${TMPDIR:-/tmp}/kapsel-demo-logs.XXXXXX")
    chmod 700 "$log_directory"
    if [[ $cluster_owned -eq 1 ]]; then
      diagnostic="$workspace/cluster-diagnostic.log"
      {
        kubectl --request-timeout=5s --kubeconfig "$workspace/kubeconfig.yaml" get all -A || true
        kubectl --request-timeout=5s --kubeconfig "$workspace/kubeconfig.yaml" get events -A || true
      } >"$diagnostic" 2>&1
      bounded_log "$diagnostic" "$log_directory/cluster-diagnostic.log"
    fi
    for log in "$workspace"/*.log; do
      [[ -e $log ]] || continue
      bounded_log "$log" "$log_directory/$(basename "$log")"
    done
    printf 'bounded demo failure logs: %s\n' "$log_directory" >&2
  fi
  if [[ $cluster_owned -eq 1 ]] && ! delete_owned_cluster; then
    [[ $status -ne 0 ]] || status=1
  fi
  if [[ -n $owned_workspace ]]; then
    if rm -rf "$owned_workspace" 2>/dev/null; then
      printf '[demo cleanup] owned workspace removed: %s\n' "$owned_workspace"
    else
      printf 'could not remove owned workspace: %s\n' "$owned_workspace" >&2
      printf 'retry only this owned path: rm -rf -- %q\n' "$owned_workspace" >&2
      [[ $status -ne 0 ]] || status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

require_versions() {
  if ! command -v docker >/dev/null 2>&1; then
    printf 'Docker is required; install Docker and start its daemon\n' >&2
    return 1
  fi
  if ! docker info >/dev/null 2>&1; then
    printf 'Docker daemon is unavailable; start the Docker daemon, then confirm `docker info` succeeds\n' >&2
    return 1
  fi
  local docker_version
  if ! docker_version=$(docker version --format '{{.Server.Version}}' 2>/dev/null) ||
    [[ ! $docker_version =~ ^[0-9]+\.[0-9]+\.[0-9]+([+._~-][0-9A-Za-z._~-]+)?$ ]]; then
    printf 'cannot parse Docker server version; confirm `docker version` reports a version\n' >&2
    return 1
  fi
  printf '[demo prerequisite] Docker server %s\n' "$docker_version"

  if ! command -v kind >/dev/null 2>&1; then
    printf 'kind is required; install kind 0.32 or newer\n' >&2
    return 1
  fi
  local kind_version
  kind_version=$(kind version)
  if [[ ! $kind_version =~ ^kind\ v([0-9]+)\.([0-9]+)\.([0-9]+)([[:space:]]|$) ]]; then
    printf 'cannot parse kind version: %s; install kind 0.32 or newer\n' "$kind_version" >&2
    return 1
  fi
  if ((BASH_REMATCH[1] == 0 && BASH_REMATCH[2] < 32)); then
    printf 'install kind 0.32 or newer; found: %s\n' "$kind_version" >&2
    return 1
  fi
  printf '[demo prerequisite] %s\n' "$kind_version"

  if ! command -v kubectl >/dev/null 2>&1; then
    printf 'kubectl is required; install kubectl 1.30 or newer\n' >&2
    return 1
  fi
  local kubectl_version
  kubectl_version=$(kubectl version --client -o json)
  if [[ ! $kubectl_version =~ \"major\":[[:space:]]*\"([0-9]+)\" ]]; then
    printf 'cannot parse kubectl major version; install kubectl 1.30 or newer\n' >&2
    return 1
  fi
  local kubectl_major=${BASH_REMATCH[1]}
  if [[ ! $kubectl_version =~ \"minor\":[[:space:]]*\"([0-9]+) ]]; then
    printf 'cannot parse kubectl minor version; install kubectl 1.30 or newer\n' >&2
    return 1
  fi
  local kubectl_minor=${BASH_REMATCH[1]}
  if ((kubectl_major < 1 || (kubectl_major == 1 && kubectl_minor < 30))); then
    printf 'install kubectl 1.30 or newer; found %s.%s\n' "$kubectl_major" "$kubectl_minor" >&2
    return 1
  fi
  printf '[demo prerequisite] kubectl %s.%s\n' "$kubectl_major" "$kubectl_minor"

  if ! command -v python3 >/dev/null 2>&1; then
    printf 'Python is required; install Python 3.11 or newer\n' >&2
    return 1
  fi
  local python_version
  if ! python_version=$(python3 -c 'import sys; assert sys.version_info >= (3, 11); print(".".join(map(str, sys.version_info[:3])))' 2>/dev/null); then
    printf 'install Python 3.11 or newer; `python3` is missing, too old, or unusable\n' >&2
    return 1
  fi
  printf '[demo prerequisite] Python %s\n' "$python_version"
}

write_json_inputs() {
  local namespace=$1
  local deployment=$2
  local operation=$3
  local authorization=$4
  local image=$5
  local prefix=$6
  cat >"$workspace/$prefix-authorization.json" <<EOF
{"authorization_id":"$authorization","operation_id":"$operation","namespace":"$namespace","deployment":"$deployment","container":"target","immutable_image_digest":"$image"}
EOF
  cat >"$workspace/$prefix-request.json" <<EOF
{"operation_id":"$operation","namespace":"$namespace","deployment":"$deployment","container":"target","immutable_image_digest":"$image"}
EOF
  chmod 600 "$workspace/$prefix-authorization.json" "$workspace/$prefix-request.json"
  "$demo_executable" provision-grant \
    --authorization "$workspace/$prefix-authorization.json" \
    --signing-seed "$workspace/signing.seed" \
    --signing-key-id demo-authorization-key \
    --output "$workspace/$prefix-grant.bin" >"$workspace/$prefix-provision.log" 2>&1
}

write_operator() {
  local prefix=$1
  local receipts=$2
  local seed=$3
  local receipt_key=$4
  local output=$5
  cat >"$output" <<EOF
{"signed_authorization_grant":"$workspace/$prefix-grant.bin","authorization_key_id":"demo-authorization-key","authorization_public_key":"$workspace/signing.pub","kubeconfig":"$workspace/kubeconfig.yaml","journal":"$workspace/$prefix-journal.sqlite3","receipt_directory":"$receipts","receipt_signing_seed":"$seed","receipt_signing_key_id":"$receipt_key"}
EOF
  chmod 600 "$output"
}

wait_marker() {
  local pid=$1
  local marker=$2
  local marker_wait_seconds=$3
  local deadline=$((SECONDS + marker_wait_seconds))
  while [[ ! -f $marker ]]; do
    if ! kill -0 "$pid" 2>/dev/null; then
      printf 'demo process exited before marker: %s\n' "$marker" >&2
      return 1
    fi
    if ((SECONDS >= deadline)); then
      printf 'timed out waiting for demo marker: %s\n' "$marker" >&2
      return 1
    fi
    sleep 0.05
  done
}

kill_at_seam() {
  local seam=$1
  local marker=$2
  local operator=$3
  local log=$4
  local marker_wait_seconds=$5
  KAPSEL_DEMO_CONTROL_DIRECTORY="$workspace/control" \
    KAPSEL_DEMO_PAUSE="$seam" \
    "$demo_executable" operate \
      --request "$workspace/failed-request.json" \
      --operator-config "$operator" >"$log" 2>&1 &
  local pid=$!
  active_child_pid=$pid
  wait_marker "$pid" "$marker" "$marker_wait_seconds"
  kill -KILL "$pid"
  if wait "$pid" 2>/dev/null; then
    printf 'demo process unexpectedly exited successfully at %s\n' "$seam" >&2
    return 1
  fi
  active_child_pid=""
  if (($(wc -c <"$log") > log_max)); then
    printf 'demo command log exceeded %s bytes\n' "$log_max" >&2
    return 1
  fi
}

printf '[demo] checking prerequisites before mutation\n'
configure_demo_inputs
require_versions
existing_clusters=$(kind get clusters)
if [[ -n $existing_clusters && $existing_clusters != 'No kind clusters found.' ]]; then
  printf 'refusing to run while kind clusters already exist:\n%s\n' "$existing_clusters" >&2
  exit 1
fi
workspace=$(mktemp -d "${TMPDIR:-/tmp}/kapsel-demo.XXXXXX")
workspace=$(cd "$workspace" && pwd -P)
chmod 700 "$workspace"
for directory in control healthy-receipts failed-receipts rotated-receipts; do
  mkdir "$workspace/$directory"
  chmod 700 "$workspace/$directory"
done

if [[ -n $source_root ]]; then
  phase 1 'building the feature-gated production executable'
  cargo build --locked --features demo-harness --bin kapsel \
    --manifest-path "$source_root/Cargo.toml"
else
  phase 1 'using the extracted feature-gated demonstration executable'
fi
phase 2 "creating disposable cluster $cluster_name"
cluster_owned=1
if ! kind create cluster --name "$cluster_name" --image "$node_image" --wait 120s; then
  printf 'kind could not prepare its node; verify that kind supports this Docker host and inspect the bounded failure logs\n' >&2
  exit 1
fi
kind get kubeconfig --name "$cluster_name" >"$workspace/kubeconfig.yaml"
chmod 600 "$workspace/kubeconfig.yaml"
phase 3 'loading pinned fixture images'
docker exec "${cluster_name}-control-plane" crictl pull "$fixture_image"
docker exec "${cluster_name}-control-plane" crictl pull "$target_image"

cat <<EOF | kubectl --kubeconfig "$workspace/kubeconfig.yaml" apply -f -
apiVersion: v1
kind: Namespace
metadata:
  name: kapsel-demo-healthy
---
apiVersion: v1
kind: Namespace
metadata:
  name: kapsel-demo-failed
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: image-demo
  namespace: kapsel-demo-healthy
spec:
  replicas: 1
  progressDeadlineSeconds: 15
  selector:
    matchLabels: {app: image-demo}
  template:
    metadata:
      labels: {app: image-demo}
    spec:
      containers:
      - {name: target, image: $fixture_image}
      - {name: untouched, image: $fixture_image}
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: image-demo-failed
  namespace: kapsel-demo-failed
spec:
  replicas: 1
  progressDeadlineSeconds: 15
  selector:
    matchLabels: {app: image-demo-failed}
  template:
    metadata:
      labels: {app: image-demo-failed}
    spec:
      containers:
      - {name: target, image: $fixture_image}
      - {name: untouched, image: $fixture_image}
EOF
kubectl --kubeconfig "$workspace/kubeconfig.yaml" -n kapsel-demo-healthy \
  rollout status deployment/image-demo --timeout=60s
kubectl --kubeconfig "$workspace/kubeconfig.yaml" -n kapsel-demo-failed \
  rollout status deployment/image-demo-failed --timeout=60s

python3 - "$workspace" "$asset_directory/kap0038-trust.hex" <<'PY'
import pathlib, sys
root = pathlib.Path(sys.argv[1])
trust_vector = pathlib.Path(sys.argv[2])
root.joinpath("signing.seed").write_bytes(bytes([9]) * 32)
root.joinpath("rotated.seed").write_bytes(bytes([8]) * 32)
root.joinpath("signing.pub").write_bytes(bytes.fromhex(
    "fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618"))
root.joinpath("receipt.trust").write_bytes(bytes.fromhex(
    trust_vector.read_text().strip()))
for path in ["signing.seed", "rotated.seed", "signing.pub", "receipt.trust"]:
    root.joinpath(path).chmod(0o600)
PY
write_json_inputs kapsel-demo-healthy image-demo demo-healthy-op demo-healthy-auth \
  "$target_image" healthy
write_json_inputs kapsel-demo-failed image-demo-failed demo-failed-op demo-failed-auth \
  "$failed_image" failed
write_operator healthy "$workspace/healthy-receipts" "$workspace/signing.seed" \
  kap0038-test-key "$workspace/healthy-operator.json"
write_operator failed "$workspace/failed-receipts" "$workspace/signing.seed" \
  kap0038-test-key "$workspace/failed-operator.json"
write_operator failed "$workspace/rotated-receipts" "$workspace/rotated.seed" \
  rotated-receipt-key "$workspace/rotated-operator.json"

phase 4 'running healthy supported-command rollout'
"$demo_executable" operate \
  --request "$workspace/healthy-request.json" \
  --operator-config "$workspace/healthy-operator.json" >"$workspace/healthy.log" 2>&1
grep -Fq '"result":"SUCCEEDED"' "$workspace/healthy.log"
untouched=$(kubectl --kubeconfig "$workspace/kubeconfig.yaml" -n kapsel-demo-healthy \
  get deployment image-demo -o jsonpath='{.spec.template.spec.containers[?(@.name=="untouched")].image}')
[[ $untouched == "$fixture_image" ]]

phase 5 'killing the failed operation after one returned mutation'
kill_at_seam after_apply "$workspace/control/after-apply.ready" \
  "$workspace/failed-operator.json" "$workspace/after-apply.log" 20
[[ $(<"$workspace/control/provider-apply-count") == 1 ]]

phase 6 'restarting, observing failed rollout, and killing after receipt commitment'
kill_at_seam after_receipt_commit "$workspace/control/after-receipt-commit.ready" \
  "$workspace/failed-operator.json" "$workspace/after-publication.log" 60
[[ $(<"$workspace/control/provider-apply-count") == 1 ]]
[[ $(find "$workspace/failed-receipts" -mindepth 1 -maxdepth 1 | wc -l) -eq 0 ]]
frozen_digest=$(python3 - "$workspace/failed-journal.sqlite3" <<'PYSQL'
import sqlite3
import sys
with sqlite3.connect(sys.argv[1]) as connection:
    print(connection.execute(
        "SELECT receipt_digest FROM kubernetes_image_operations WHERE state = 'finalized'"
    ).fetchone()[0])
PYSQL
)

phase 7 'restarting under rotated receipt settings'
"$demo_executable" operate \
  --request "$workspace/failed-request.json" \
  --operator-config "$workspace/rotated-operator.json" >"$workspace/rotated.log" 2>&1
grep -Fq '"state":"FINALIZED"' "$workspace/rotated.log"
grep -Fq '"result":"FAILED"' "$workspace/rotated.log"
[[ $(find "$workspace/rotated-receipts" -maxdepth 1 -type f -name '*.receipt' | wc -l) -eq 1 ]]
frozen_receipt=$(find "$workspace/rotated-receipts" -maxdepth 1 -type f -name '*.receipt' -print)
[[ $(sha256_file "$frozen_receipt") == "$frozen_digest" ]]
[[ $(<"$workspace/control/provider-apply-count") == 1 ]]

phase 8 'deleting the owned cluster and inspecting the frozen receipt offline'
delete_owned_cluster
KUBECONFIG=/unavailable/ambient-kubeconfig HTTPS_PROXY=http://127.0.0.1:1 \
  "$demo_executable" inspect \
    --receipt "$frozen_receipt" \
    --trust "$workspace/receipt.trust" \
    --evaluation-time-unix-s 150 >"$workspace/inspection.log" 2>&1
grep -Fq '"status":"INSPECTED"' "$workspace/inspection.log"
grep -Fq '"result":"FAILED"' "$workspace/inspection.log"
grep -Fq '"rollout_condition_reason":"ProgressDeadlineExceeded"' \
  "$workspace/inspection.log"
grep -Fq '"non_claims":"no-exactly-once;' "$workspace/inspection.log"
if grep -Fq VERIFIED "$workspace/inspection.log"; then
  printf 'offline inspection emitted forbidden VERIFIED vocabulary\n' >&2
  exit 1
fi

phase 9 'showing classifier-complete inspection and lifecycle evidence'
printf 'offline inspection report:\n'
tail -c "$log_max" "$workspace/inspection.log"
printf 'Kapsel lifecycle evidence summary:\n'
printf '  healthy receiver outcome: SUCCEEDED\n'
printf '  durable attempt: apply_started recorded before provider mutation\n'
printf '  process termination: after the returned mutation\n'
printf '  restart behavior: reconciled without a blind second mutation\n'
printf '  provider apply count: 1\n'
printf '  process termination: after frozen receipt commitment\n'
printf '  frozen receipt: finalized unchanged under rotated receipt settings\n'
printf '  frozen receipt sha256: %s\n' "$frozen_digest"
printf '  receiver outcome: FAILED from ProgressDeadlineExceeded\n'
printf '  offline inspection path: %s (temporary until cleanup)\n' "$frozen_receipt"
printf '  offline inspection: INSPECTED, never VERIFIED\n'
printf '  UNKNOWN boundary: incomplete or ambiguous bounded observation remains UNKNOWN\n'
printf '  non-claims: no exactly-once effect, causation, Kubernetes truth, complete capture, witnessing, or production readiness\n'
printf '  elapsed: %ss before owned workspace cleanup\n' "$((SECONDS - demo_start_seconds))"
