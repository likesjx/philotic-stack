#!/bin/sh
# Run only in a disposable x86_64 Linux build workspace with cached dependencies.
set -eu
[ "${1-}" = "--build-offline" ] || { echo 'Inert: pass --build-offline on an isolated Linux x86_64 builder.'; exit 0; }
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || { echo 'Linux x86_64 builder required'; exit 1; }
cd "$(dirname "$0")"
command -v cargo >/dev/null
command -v cc >/dev/null
cargo test --offline --locked --manifest-path broker-helper/Cargo.toml -- --test-threads=1
cargo build --offline --locked --release --target x86_64-unknown-linux-gnu --manifest-path broker-helper/Cargo.toml
cargo build --offline --locked --release --target x86_64-unknown-linux-gnu --manifest-path broker-helper/Cargo.toml --example private_start_fixture
python3 - <<'CHECK'
from pathlib import Path
import hashlib,json,os,subprocess
source=Path('broker-helper/target/x86_64-unknown-linux-gnu/release/synthetic-broker-helper')
data=source.read_bytes()
assert data[:6]==b'\x7fELF\x02\x01' and data[18:20]==b'\x3e\x00'
# Default invocation must refuse, without opening a live DB or reading credentials.
p=subprocess.run([str(source.resolve())],capture_output=True)
assert p.returncode==1 and not p.stdout
assert p.stderr==b'scoped broker transaction stopped; reconcile journal\n'
out=Path('broker-helper-linux-amd64')
with out.open('xb') as f:f.write(data)
os.chmod(out,0o755)
Path('linux-build-receipt.json').write_text(json.dumps({'sha256':hashlib.sha256(data).hexdigest(),'format':'ELF64 x86_64','build':'offline locked release','native_linux_tests':'passed','default_invocation':'refused before runtime access','runtime_acceptance':False},indent=2)+'\n')
CHECK
