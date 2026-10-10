#!/usr/bin/env bash
# Runs the already compiled native Rust owner integration test in an isolated
# synthetic graph. Binary build command is documented; never builds a live client.
set -euo pipefail
[[ ${1:-} == --approved-fixture-run && $# == 2 ]] || { echo 'Requires approval flag and compiled owner test binary.' >&2; exit 2; }
[[ $(uname -s) == Linux && $(uname -m) == x86_64 && -f $2 ]] || exit 2
binary=$(realpath -- "$2")
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
fixture_file="$script_dir/../lifegraph-memgraph-tests/fixture.cypher"
graph_name=lifegraph-owner-synthetic-3101
graph_image=memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4
if docker inspect "$graph_name" >/dev/null 2>&1; then echo 'Refusing existing container.' >&2; exit 2; fi
created=false
cleanup() { if "$created"; then docker rm -f "$graph_name" >/dev/null; fi; }
trap cleanup EXIT
if ! docker image inspect "$graph_image" >/dev/null 2>&1; then docker pull "$graph_image" >/dev/null; fi
docker run -d --name "$graph_name" --network none --cap-drop ALL --security-opt no-new-privileges \
  --memory 1g --cpus 1 --pids-limit 128 --tmpfs /var/lib/memgraph:rw,uid=101,gid=103 \
  --tmpfs /var/log/memgraph:rw,uid=101,gid=103 "$graph_image" --log-level=WARNING \
  --storage-wal-enabled=false --storage-snapshot-interval-sec=0 --telemetry-enabled=false >/dev/null
created=true
docker cp "$binary" "$graph_name:/tmp/lifegraph-owner-tests"
ready=false
for _ in {1..30}; do
  if docker exec -i "$graph_name" mgconsole --host 127.0.0.1 --port 7687 <<< 'RETURN 1;' >/dev/null 2>&1; then ready=true; break; fi
  sleep 1
done
"$ready" || { echo 'Disposable graph startup failed.' >&2; exit 1; }
docker exec -i "$graph_name" mgconsole --host 127.0.0.1 --port 7687 < "$fixture_file" >/dev/null
# Exact scope: one synthetic integration test and its own supervised test children.
docker exec --user 101:103 --env LIFEGRAPH_APPROVED_DISPOSABLE_BOLT=memgraph-3.10.1 "$graph_name" \
  /tmp/lifegraph-owner-tests --ignored --exact read_owner::tests::real_disposable_memgraph_snapshot_holds_release_fence
