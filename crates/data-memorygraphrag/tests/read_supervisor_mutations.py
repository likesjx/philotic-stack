#!/usr/bin/env python3
"""Explicit synthetic sensitivity controls; mutate only disposable git exports.

No databases, host ports, credentials or production activation. Positive tests
must pass first. Build failures never count as successful mutation detection.
"""
import argparse
import pathlib
import subprocess
import tempfile

IMAGE = 'rust@sha256:8e0f5926ce10ec99e12703b315c0043e4709d6bdc740dc1d28753e5fb87c766f'

def replace(path, old, new):
    source = path.read_text()
    assert source.count(old) == 1, 'Mutation anchor changed'
    path.write_text(source.replace(old, new))

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--approved-fixture-run', action='store_true', required=True)
    p.add_argument('--source', type=pathlib.Path, required=True)
    p.add_argument('--target', type=pathlib.Path, required=True,
                   help='Disposable mutation output directory, never the positive artifact directory')
    p.add_argument('--registry', type=pathlib.Path, required=True)
    args = p.parse_args()
    source = args.source.resolve(strict=True)
    assert args.target.resolve() != source and args.target.resolve() != source/'target'
    args.target.mkdir(parents=True, exist_ok=True)
    assert not subprocess.check_output(['git', '-C', str(source), 'status', '--porcelain'])
    head = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
    for mutation in ('auth', 'fence'):
        with tempfile.TemporaryDirectory(prefix='synthetic-supervisor-mutation-') as temporary:
            checkout = pathlib.Path(temporary)
            archive = subprocess.run(['git', '-C', str(source), 'archive', head], check=True, capture_output=True).stdout
            subprocess.run(['tar', '-x', '-C', str(checkout)], input=archive, check=True)
            owner = checkout/'crates/data-memorygraphrag/src/read_owner.rs'
            if mutation == 'auth':
                replace(checkout/'crates/ansible-mesh-core/src/privacy_local.rs',
                        '|| launch.process.pid()? != self.pid', '|| false /* MUTANT: omit kernel PID binding */')
                replace(owner, 'for (guest, role) in roles.iter() {',
                        'for (guest, role) in roles.iter() { if *role != OwnerRole::Issuer { continue; }')
                tests = ['unenrolled_peer_cannot_begin_a_mutation']
                expected = 'unenrolled peer was not explicitly rejected'
            else:
                replace(owner, 'gate = Some(lock);', '''gate = Some(lock);
                    gate.as_mut().unwrap().complete = true;
                    drop(gate.take());
                    eprintln!("LifeGraph owner fence contended");
                    /* MUTANT: drop real fence immediately, but retain observer event */''')
                tests = ['issuer_lease_also_blocks_gateway_release',
                         'real_children_serialize_and_clean_shutdown_can_restart']
                expected = 'contending lease was admitted'
            docker = ['docker', 'run', '--rm', '--network', 'none', '--cpus', '2', '--memory', '3g',
                      '-v', f'{checkout}:/source', '-v', f'{args.target.resolve()}:/target',
                      '-v', f'{args.registry.resolve()}:/usr/local/cargo/registry',
                      '-e', 'CARGO_TARGET_DIR=/target', '-e', 'CARGO_BUILD_JOBS=2',
                      '-e', 'CARGO_INCREMENTAL=0', '-e', 'CARGO_PROFILE_TEST_DEBUG=0',
                      '-e', 'CARGO_PROFILE_DEV_DEBUG=0',
                      '-e', 'RUSTC=/usr/local/rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/bin/rustc',
                      '-w', '/source', IMAGE]
            cargo = '/usr/local/rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/bin/cargo'
            common = ['--offline', '--locked', '-p', 'data-memorygraphrag', '--test', 'read_supervisor']
            subprocess.run(docker+[cargo, 'test', *common, '--no-run'], check=True, timeout=900)
            for test in tests:
                result = subprocess.run(docker+[cargo, 'test', *common, test, '--', '--exact', '--test-threads=1'],
                                        capture_output=True, text=True, timeout=120)
                output = result.stdout+result.stderr
                assert result.returncode != 0 and expected in output, output
                print(f'{mutation}/{test}: intended assertion rejected guard-disabled export ({head})', flush=True)

if __name__ == '__main__':
    main()
