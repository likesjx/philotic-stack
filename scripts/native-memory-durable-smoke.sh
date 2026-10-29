#!/usr/bin/env bash
# CI-only synthetic SIGKILL recovery qualification. No physical-power-loss,
# disk-failure, host-reboot or production durability guarantee is established.
set -euo pipefail
[[ "${GITHUB_ACTIONS:-}" == true && "${CI:-}" == true ]] || {
  echo 'This fixture is restricted to the existing GitHub Actions acceptance job.' >&2
  exit 1
}
cd "$(dirname "$0")/.."
readonly image='memgraph/memgraph-mage@sha256:e8e03640071c57de6a13f2f4c5f9ed1789797dc5e8e771bbe62203b20e1b8c27'
readonly fixture="native-memory-durable-${GITHUB_RUN_ID:?}-${GITHUB_RUN_ATTEMPT:?}-${RANDOM}"
readonly volume="${fixture}-data"
container_created=false
volume_created=false
cleanup() {
  if "$container_created"; then
    docker logs "$fixture" || true
    docker rm -f "$fixture" >/dev/null || true
  fi
  if "$volume_created"; then docker volume rm "$volume" >/dev/null || true; fi
}
trap cleanup EXIT
# Never pull/install an image; reuse the preceding acceptance step's pinned image.
docker image inspect "$image" >/dev/null
if docker container inspect "$fixture" >/dev/null 2>&1 || \
    docker volume inspect "$volume" >/dev/null 2>&1; then
  echo 'Refusing to reuse an existing fixture container or volume.' >&2
  exit 1
fi
docker volume create "$volume" >/dev/null
volume_created=true
container_created=true
docker run -d --pull never --name "$fixture" \
  --publish 127.0.0.1:17687:7687 \
  --cap-drop ALL --security-opt no-new-privileges \
  --memory 1g --cpus 1 --pids-limit 128 \
  --mount "type=volume,source=$volume,target=/var/lib/memgraph" \
  --tmpfs /var/log/memgraph:rw,uid=101,gid=103 \
  "$image" --log-level=WARNING --telemetry-enabled=false \
  --data-recovery-on-startup=true --storage-wal-enabled=true \
  --storage-wal-file-flush-every-n-tx=1 \
  --storage-snapshot-interval-sec=300 --storage-snapshot-on-exit=true >/dev/null
# Current flags: https://memgraph.com/docs/database-management/configuration
ready() {
  for _ in {1..60}; do
    [[ "$(docker inspect --format '{{.State.Running}}' "$fixture")" == true ]] || return 1
    if docker exec -i "$fixture" mgconsole --host 127.0.0.1 --port 7687 \
        <<< 'RETURN 1;' >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  echo 'Disposable Memgraph did not become ready within 60 seconds.' >&2
  return 1
}
ready
export PHILOTIC_SYNTHETIC_NATIVE_DURABLE_MEMGRAPH=1
# Separate cargo invocations start separate test/adapter processes.
timeout 600 cargo test --locked -p native-memory --test durable_memgraph \
  durable_seed -- --ignored --exact --test-threads=1
# Require actual non-empty snapshot AND WAL files before the crash, rather than
# claiming durability solely from command-line flags or in-process reconnection.
for directory in snapshots wal; do
  files=$(docker exec "$fixture" find /var/lib/memgraph -type f -path "*/$directory/*" -size +0c -print)
  [[ -n "$files" ]] || { echo "Missing non-empty $directory artifact" >&2; exit 1; }
  printf '%s\n' "$files"
done
docker kill --signal KILL "$fixture" >/dev/null
[[ "$(docker inspect --format '{{.State.ExitCode}}' "$fixture")" == 137 ]]
docker start "$fixture" >/dev/null
ready
timeout 600 cargo test --locked -p native-memory --test durable_memgraph \
  durable_verify -- --ignored --exact --test-threads=1
echo 'Snapshot/WAL SIGKILL restart qualification passed; physical power loss is not qualified.'
