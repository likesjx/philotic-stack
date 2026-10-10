#!/usr/bin/env bash
# Explicit disposable acceptance only. Never points at a supplied graph address.
set -euo pipefail
if [[ ${1:-} != --approved-fixture-run ]]; then
  echo 'Requires --approved-fixture-run and approval for these disposable image/dependency downloads.' >&2
  exit 2
fi
[[ $(uname -m) == x86_64 && $(uname -s) == Linux ]] || { echo 'Pinned recipe is linux/amd64 only.' >&2; exit 2; }
fixture_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
service_dir=$(dirname -- "$fixture_dir")
graph_name=lifegraph-synthetic-3101
build_name=lifegraph-synthetic-rust-build
graph_image=memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4
rust_image=rust@sha256:8e0f5926ce10ec99e12703b315c0043e4709d6bdc740dc1d28753e5fb87c766f
for name in "$graph_name" "$build_name"; do
  if docker inspect "$name" >/dev/null 2>&1; then echo "Refusing existing container: $name" >&2; exit 2; fi
done
test_tmp=$(mktemp -d)
created_graph=false
created_build=false
cleanup() {
  if "$created_graph"; then docker rm -f "$graph_name" >/dev/null; fi
  if "$created_build"; then docker rm -f "$build_name" >/dev/null; fi
  rm -rf -- "$test_tmp"
}
trap cleanup EXIT
docker pull "$graph_image"
docker pull "$rust_image"
source_uid=$(stat -c %u "$fixture_dir/Cargo.toml")
source_gid=$(stat -c %g "$fixture_dir/Cargo.toml")
docker create --name "$build_name" --user "$source_uid:$source_gid" --cap-drop ALL \
  --security-opt no-new-privileges --memory 2g --cpus 2 --pids-limit 256 \
  --env CARGO_HOME=/tmp/cargo --env CARGO_HTTP_CAINFO=/tmp/cloud-ca.crt --workdir /fixture \
  "$rust_image" cargo build --release --locked --target-dir /tmp/lifegraph-target
created_build=true
docker cp "$fixture_dir/." "$build_name:/fixture"
# Public existing CA bundle only; no TLS bypass, host trust changes or secrets.
docker cp /etc/ssl/certs/ca-certificates.crt "$build_name:/tmp/cloud-ca.crt"
docker start -a "$build_name"
docker cp "$build_name:/tmp/lifegraph-target/release/lifegraph-synthetic-bolt-fixture" "$test_tmp/binary"
chmod 755 "$test_tmp/binary"
docker run -d --name "$graph_name" --network none --cap-drop ALL --security-opt no-new-privileges \
  --memory 1g --cpus 1 --pids-limit 128 --tmpfs /var/lib/memgraph:rw,uid=101,gid=103 \
  --tmpfs /var/log/memgraph:rw,uid=101,gid=103 "$graph_image" --log-level=WARNING \
  --storage-wal-enabled=false --storage-snapshot-interval-sec=0 --telemetry-enabled=false
created_graph=true
docker cp "$test_tmp/binary" "$graph_name:/tmp/lifegraph-synthetic-bolt-fixture"
ready=false
for _ in {1..30}; do
  if docker exec -i "$graph_name" mgconsole --host 127.0.0.1 --port 7687 <<< 'RETURN 1;' >/dev/null 2>&1; then ready=true; break; fi
  sleep 1
done
"$ready" || { echo 'Disposable graph startup failed.' >&2; exit 1; }
docker exec -i "$graph_name" mgconsole --host 127.0.0.1 --port 7687 < "$fixture_dir/fixture.cypher"
(cd -- "$service_dir" && node --test lifegraph-memgraph.acceptance.mjs)
