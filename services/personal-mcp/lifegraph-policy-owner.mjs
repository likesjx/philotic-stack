// Lease provider over the canonical, existing dedicated privacy store. Linux
// /proc/self/fd avoids SQLite's CREATE flag reopening a replaced/missing path.
// No policies, grants, schema or revisions are written by this owner.
import { DatabaseSync } from 'node:sqlite';
import { constants, openSync, closeSync, fstatSync, lstatSync } from 'node:fs';
import { isAbsolute } from 'node:path';
const fail = () => { throw new Error('LifeGraph canonical policy unavailable'); };
const identity = stat => ({ device: String(stat.dev), inode: String(stat.ino) });
export function createLifeGraphCanonicalPolicyOwner({ policyDatabase, storeIdentity }) {
  if (process.platform !== 'linux' || typeof policyDatabase !== 'string' || !isAbsolute(policyDatabase) ||
      policyDatabase.includes('\0') || !storeIdentity || typeof storeIdentity.device !== 'string' ||
      typeof storeIdentity.inode !== 'string') fail();
  let closed = false;
  const expected = Object.freeze({ device: storeIdentity.device, inode: storeIdentity.inode });
  const active = new Set();
  function verify(stat) {
    if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o077) !== 0 ||
        JSON.stringify(identity(stat)) !== JSON.stringify(expected)) fail();
  }
  return Object.freeze({
    async pin({ signal } = {}) {
      if (closed || active.size >= 32) fail();
      signal?.throwIfAborted(); let fd, db;
      try {
        verify(lstatSync(policyDatabase));
        fd = openSync(policyDatabase, constants.O_RDWR | constants.O_NOFOLLOW);
        verify(fstatSync(fd)); verify(lstatSync(policyDatabase));
        db = new DatabaseSync(`/proc/self/fd/${fd}`, { timeout: 100 });
        if (db.prepare('PRAGMA journal_mode').get().journal_mode !== 'delete') fail();
        if (db.prepare('PRAGMA user_version').get().user_version !== 2) fail();
        db.prepare('SELECT resource,policy_json FROM privacy_policy LIMIT 0').all();
        db.prepare('SELECT hotel,origin,task,handle,binding,cancelled FROM local_authority_receipt LIMIT 0').all();
        db.prepare('SELECT producer,event_id,actor,payload,sources_json,policy_revision FROM capture_inbox LIMIT 0').all();
        db.exec('BEGIN IMMEDIATE');
        const rows = db.prepare('SELECT singleton,revision FROM privacy_revision').all();
        if (rows.length !== 1 || rows[0].singleton !== 1 || !Number.isSafeInteger(rows[0].revision) || rows[0].revision < 1) fail();
        verify(fstatSync(fd)); verify(lstatSync(policyDatabase)); signal?.throwIfAborted();
        let released = false;
        const lease = Object.freeze({ revision: String(rows[0].revision),
          validate() { if (released) return false; try { verify(fstatSync(fd)); verify(lstatSync(policyDatabase)); return true; } catch { return false; } },
          release() {
            if (released) return; released = true; active.delete(lease);
            try { db.exec('ROLLBACK'); } finally { try { db.close(); } finally { closeSync(fd); } }
          } });
        active.add(lease); return lease;
      } catch {
        try { db?.close(); } finally { if (fd !== undefined) closeSync(fd); }
        fail();
      }
    },
    // Closing cannot shorten an already admitted response's reservation.
    close() { closed = true; if (active.size) fail(); },
  });
}
