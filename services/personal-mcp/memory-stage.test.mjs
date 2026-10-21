import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdtemp, cp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';

const role = new URL('../../ansible/roles/percival_personal_mcp/', import.meta.url);
const source = new URL('.', import.meta.url);
const text = await readFile(new URL('tasks/memory_stage.yml', role), 'utf8');
const files = JSON.parse(text.match(/^    memory_files: (.+)$/m)[1]);

test('memory stage pins the exact complete gateway source and qualified Node', async () => {
  assert.equal(Object.keys(files).length, 17);
  assert.equal(files['bin/node'], '7fde7b8afa198da66257f42ee2001d874c7355631e6d1579a5fb5ef1f246df4c');
  assert.ok(files['gateway/whole-vault.mjs']);
  for (const [path, digest] of Object.entries(files)) {
    if (!path.startsWith('gateway/')) continue;
    const actual = createHash('sha256').update(await readFile(new URL(path.slice(8), source))).digest('hex');
    assert.equal(actual, digest, path);
  }
});

test('staged closure actually constructs whole-vault gateway without listeners', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'muninn-stage-closure-'));
  try {
    for (const path of Object.keys(files).filter(p => p.startsWith('gateway/'))) await cp(new URL(path.slice(8), source), join(dir, path.slice(8)));
    const script = `
      import assert from 'node:assert/strict';
      import { createPersonalMcp } from './gateway.mjs';
      const clients = ['claude_muninn_recall_v1','chatgpt_muninn_recall_v1'];
      const server = await createPersonalMcp({resource:'https://fixture.example/personal/mcp',
        muninnVault:'default', enabledTools:['muninn_recall'],
        allowedSubjects:new Set(['synthetic-subject']), allowedClients:new Set(clients),
        wholeVaultAccess:{version:1,subject:'synthetic-subject',vault:'default',includeFutureRecords:true},
        clientPolicies:clients.map(clientId=>({clientId,subjects:['synthetic-subject'],vault:'default',scopes:['memory:recall']})),
        issuer:{issuer:'https://issuer.example',async ready(){}, async introspect(){throw Error('not called');}},
        upstream:{async list(){return {description:'synthetic',inputSchema:{type:'object'}}}, async call(){throw Error('not called');}}
      });
      assert.equal(server.listening,false);
    `;
    await writeFile(join(dir, 'construct.mjs'), script);
    const run = () => spawnSync(process.execPath, ['construct.mjs'], {cwd:dir, encoding:'utf8', timeout:5000});
    assert.equal(run().status, 0);
    await rm(join(dir, 'whole-vault.mjs'));
    const missing = run();
    assert.notEqual(missing.status, 0);
    assert.match(missing.stderr, /whole-vault.mjs/);
  } finally { await rm(dir, {recursive:true, force:true}); }
});

test('memory staging tasks have no service, account, activation or execution surface', async () => {
  const verify = await readFile(new URL('tasks/memory_verify_stage.yml', role), 'utf8');
  const modules = [...(text + verify).matchAll(/ansible\.builtin\.(\w+):/g)].map(x=>x[1]);
  for (const module of modules) assert.ok(['assert','stat','file','copy','find','include_tasks','debug'].includes(module), module);
  assert.doesNotMatch(text + verify, /^\s*notify:/m);
  assert.doesNotMatch(text + verify, /state: link|dest: \/etc\//);
  assert.match(text, /percival_memory_operation == 'stage'/);
  assert.match(text, /Memory activation is unavailable/);
});
