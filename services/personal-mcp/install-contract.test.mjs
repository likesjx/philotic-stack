import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, cp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const role = new URL('../../ansible/roles/percival_personal_mcp/', import.meta.url);
const source = new URL('.', import.meta.url);
test('exact role package resolves all transitive gateway imports without starting services', async () => {
  const tasks = await readFile(new URL('tasks/main.yml', role), 'utf8');
  const loops = [...tasks.matchAll(/loop: \[([^\]]+)\]/g)].map(x => x[1].split(',').map(x => x.trim()));
  const files = loops.filter(x => x.includes('server.mjs'));
  assert.equal(files.length, 2);
  assert.deepEqual(files[0], files[1]);
  const dir = await mkdtemp(join(tmpdir(), 'muninn-install-fixture-'));
  try {
    for (const file of files[0]) await cp(new URL(file, source), join(dir, file));
    const result = spawnSync(process.execPath, ['--input-type=module', '-e', "await import('./server.mjs'); await import('./gateway.mjs')"], { cwd: dir, timeout: 5000, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('role requires independently explicit UID/GID and remains disabled memory-only', async () => {
  const defaults = await readFile(new URL('defaults/main.yml', role), 'utf8');
  const tasks = await readFile(new URL('tasks/main.yml', role), 'utf8');
  assert.match(defaults, /percival_enabled: false/);
  assert.match(defaults, /percival_gateway_uid: null/);
  assert.match(defaults, /percival_gateway_gid: null/);
  assert.match(tasks, /percival_gateway_gid is integer/);
  assert.match(tasks, /gid: '\{\{ percival_gateway_gid \}\}'/);
  assert.match(tasks, /getent_passwd\[percival_gateway_user\]\[2\] \| int == percival_gateway_gid/);
  assert.match(tasks, /enabledTools == \['muninn_recall'\]/);
  assert.match(tasks, /muninnVault == 'percival_connection_test'/);
});
