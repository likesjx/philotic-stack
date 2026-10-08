"""Only disposable container FDs; no hotel, account, credential or network."""
import os, pathlib, socket, subprocess
assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
assert pathlib.Path('/.dockerenv').exists()
path = pathlib.Path('/run/percival-personal-vault-broker.sock')
binary = '/test-target/debug/activation-exec-fixture'
base = {'PATH': '/usr/bin:/bin', 'PERCIVAL_ISOLATED_LINUX_FIXTURE': '1'}
try:
    path.unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(str(path)); listener.listen(8)
        fd = listener.fileno()
        names = ':'.join(['unrelated'] * (fd - 3) + ['percival-scoped-vault'])
        def run(mode, metadata=True, names_value=names, supply=True, extra_alias=False):
            env = dict(base)
            if mode is not None: env['PHILOTIC_SCOPED_VAULT_ENABLED'] = mode
            if metadata:
                env.update(LISTEN_PID='fixture', LISTEN_FDS=str(fd - 2), LISTEN_FDNAMES=names_value,
                           PERCIVAL_FIXTURE_SELF_PID='1')
            alias = os.dup(fd) if extra_alias else None
            try:
                inherited = ((fd,) if supply else ()) + ((alias,) if alias is not None else ())
                return subprocess.run([binary], env=env, pass_fds=inherited,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=3)
            finally:
                if alias is not None: os.close(alias)
        result = run('1')
        assert result.returncode == 0, result.stderr.decode()
        assert b'guest-exec-checked-no-scoped-descriptor' in result.stdout
        result = run('1', extra_alias=True)
        assert result.returncode == 3, result.stderr.decode()
        assert b'activation-rejected-before-guest-exec' in result.stdout
        for mode in ('0', None):
            for metadata, names_value in ((True, names), (True, 'wrong-name'), (False, '')):
                result = run(mode, metadata, names_value)
                assert result.returncode == 3, (mode, metadata, result.stderr.decode())
                assert b'activation-rejected-before-guest-exec' in result.stdout
                assert b'guest-exec' not in result.stdout.replace(b'before-guest-exec', b'')
            result = run(mode, metadata=False, supply=False)
            assert result.returncode == 0, result.stderr.decode()
            assert b'guest-exec-checked-no-scoped-descriptor' in result.stdout
    print('PASS actual optional activation: enabled original/duplicate FDs absent after exec and extra unadvertised alias rejected; disabled/unset with scoped FD or mismatched/missing metadata rejected before guest exec; clean disabled exec has no scoped FD')
finally:
    path.unlink(missing_ok=True)
