#!/bin/bash
# Proposed disposable CI build only. No installed executable or live state input.
set -euo pipefail
test -f /.dockerenv
test "$(id -u)" = 0
test "$PERCIVAL_ISOLATED_BUILD" = 1
export HOME=/root PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export DEBIAN_FRONTEND=noninteractive CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0 CMAKE_POLICY_VERSION_MINIMUM=3.5
apt-get update
apt-get install -y --no-install-recommends ca-certificates curl git build-essential pkg-config libssl-dev libopus-dev python3
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh
sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.94.1
groupadd -g 987 fixture-hotel
useradd -u 999 -g 987 --no-create-home --shell /usr/sbin/nologin fixture-hotel
mkdir /candidate /historical /binaries /evidence
tar -C /input-candidate --exclude=.git -cf - . | tar -C /candidate -xf -
tar -C /input-historical --exclude=.git -cf - . | tar -C /historical -xf -
cd /historical
mkdir -p crates/philotic-client/examples
cp /candidate/services/personal-mcp/qualification/guest.rs crates/philotic-client/examples/qualification_guest.rs
cargo build --locked --release --target-dir /guest-target -p philotic-client --example qualification_guest
cargo build --locked --release --target-dir /historical-target -p aiua
cp /historical-target/release/aiua /binaries/historical
cd /candidate
export PHILOTIC_BUILD_SHA="$QUALIFICATION_SOURCE_SHA"
cargo build --locked --release --target-dir /hotel-target -p aiua
cp /hotel-target/release/aiua /binaries/candidate
mkdir -p /hotel-target/debug
ln -s ../release/aiua /hotel-target/debug/aiua
cargo test --locked --release --target-dir /hotel-target -p aiua --no-run --message-format=json > /evidence/unit-compile.json
python3 - <<'PY'
import json,pathlib
paths=[x['executable'] for line in pathlib.Path('/evidence/unit-compile.json').read_text().splitlines() if (x:=json.loads(line)).get('reason')=='compiler-artifact' and x.get('profile',{}).get('test') and x.get('target',{}).get('name')=='aiua' and x.get('executable')]
assert len(paths)==1
pathlib.Path('/evidence/unit-executable.txt').write_text(paths[0])
PY
cargo build --locked --manifest-path services/personal-mcp/scoped-vault-tests/Cargo.toml --target-dir /test-target
cargo test --locked --manifest-path services/personal-mcp/scoped-vault-tests/Cargo.toml --target-dir /test-target --no-run
cargo build --locked --release --manifest-path services/personal-mcp/synthetic-broker-build/broker-helper/Cargo.toml --bin persistent-broker-helper --target-dir /helper-target
cp /binaries/candidate /evidence/aiua
cp /helper-target/release/persistent-broker-helper /evidence/persistent-broker-helper
sha256sum /binaries/* /helper-target/release/persistent-broker-helper Cargo.lock services/personal-mcp/synthetic-broker-build/broker-helper/Cargo.lock > /evidence/build-inputs.sha256
rustc -vV > /evidence/rustc.txt
printf '%s\n' "$QUALIFICATION_SOURCE_SHA" > /evidence/candidate-source.txt
