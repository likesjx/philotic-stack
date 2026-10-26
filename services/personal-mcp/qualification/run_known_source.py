"""Network-isolated disposable candidate qualification; never an operator tool."""
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import subprocess
import sys


def main():
    assert sys.platform == 'linux' and os.geteuid() == 0
    assert Path('/.dockerenv').is_file()
    assert set(os.listdir('/sys/class/net')) == {'lo'}
    assert pwd.getpwuid(999).pw_gid == 987
    assert not Path('/opt/philotic').exists()
    assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
    fixtures = Path('/candidate/services/personal-mcp/scoped-vault-tests')
    common = {'PATH': '/usr/bin:/bin', 'PERCIVAL_ISOLATED_LINUX_FIXTURE': '1',
              'PERCIVAL_BOOTSTRAP_FIXTURES': str(fixtures),
              'QUALIFICATION_CANDIDATE_SHA256': hashlib.sha256(Path('/binaries/candidate').read_bytes()).hexdigest(),
              'QUALIFICATION_HISTORICAL_SHA256': hashlib.sha256(Path('/binaries/historical').read_bytes()).hexdigest()}
    cases = []

    def run(name, argv, env=common, timeout=300, expected_count=None):
        try:
            result = subprocess.run(argv, env=env, timeout=timeout,
                                    stdout=subprocess.PIPE if expected_count else None,
                                    stderr=subprocess.STDOUT if expected_count else None,
                                    text=True)
        except subprocess.TimeoutExpired:
            cases.append({'name': name, 'exit': None, 'outcome': 'timeout'})
            raise
        except OSError:
            cases.append({'name': name, 'exit': None, 'outcome': 'start_failed'})
            raise
        case = {'name': name, 'exit': result.returncode, 'outcome': 'passed' if result.returncode == 0 else 'failed'}
        cases.append(case)
        if expected_count:
            print(result.stdout, flush=True)
            if not re.search(r'^test result: ok\. ' + str(expected_count) + r' passed; 0 failed; 0 ignored;', result.stdout, re.M):
                case['outcome'] = 'test_count_mismatch'
                raise AssertionError('migration test count mismatch')

    completed = False
    try:
        unit = Path('/evidence/unit-executable.txt').read_text().strip()
        home = Path('/tmp/public-migration-unit-home')
        home.mkdir()
        run('atomic_credential_migration', [unit, 'provider_api_key_migration', '--test-threads=1'],
            {**common, 'HOME': str(home), 'PHILOTIC_KEYCHAIN_ENABLED': '0',
             'PHILOTIC_VAULT_MASTER_KEY': 'BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc='}, expected_count=3)
        run('source_protocol_and_activation', [sys.executable, str(fixtures / 'run_linux_ci.py')])
        run('actual_helper_and_hotel_production_identity_contract',
            [sys.executable, str(fixtures / 'hotel_bootstrap_acceptance.py')],
            {**common, 'PERCIVAL_KNOWN_SOURCE_CONTRACT': '1'})
        run('historical_guest_state_interruption_and_rollback',
            [sys.executable, '/candidate/services/personal-mcp/qualification/compatibility.py'])
        assert len(cases) == 4 and all(case['outcome'] == 'passed' for case in cases)
        assert not Path('/opt/philotic').exists()
        completed = True
    finally:
        Path('/evidence/known-source-results.json').write_text(json.dumps({
            'syntheticOnly': True, 'productionUsed': False, 'installedEquivalence': False,
            'completed': completed, 'cases': cases, 'cleanupRootAbsent': not Path('/opt/philotic').exists(),
            'allInstalledGuestsQualified': False,
        }, sort_keys=True, indent=2) + '\n')


if __name__ == '__main__':
    main()
