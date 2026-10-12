// Read-only loading of an owner-approved export of the canonical recall catalog.
// This does not create a second policy store or infer bindings from live records.
import { constants, openSync, closeSync, fstatSync, readSync } from 'node:fs';
import { isAbsolute } from 'node:path';
import { createHash } from 'node:crypto';
const fail = () => { throw new Error('LifeGraph catalog unavailable'); };
export function loadLifeGraphCatalog({ catalogFile, expectedDigest }) {
  if (typeof catalogFile !== 'string' || !isAbsolute(catalogFile) || catalogFile.includes('\0') ||
      !/^[a-f0-9]{64}$/.test(expectedDigest ?? '')) fail();
  let fd;
  try {
    fd = openSync(catalogFile, constants.O_RDONLY | constants.O_NOFOLLOW);
    const before = fstatSync(fd);
    if (!before.isFile() || before.uid !== process.getuid() || (before.mode & 0o077) !== 0 ||
        before.size < 1 || before.size > 1048576) fail();
    const bytes = Buffer.alloc(before.size); let read = 0;
    while (read < bytes.length) { const n = readSync(fd, bytes, read, bytes.length-read, read); if (n === 0) fail(); read += n; }
    const after = fstatSync(fd);
    if (after.size !== before.size || after.mtimeMs !== before.mtimeMs || after.ctimeMs !== before.ctimeMs ||
        createHash('sha256').update(bytes).digest('hex') !== expectedDigest) fail();
    const value = JSON.parse(bytes.toString('utf8'));
    if (value?.version !== 1 || Object.keys(value).some(k => !['version','catalogs'].includes(k)) ||
        !Array.isArray(value.catalogs) || value.catalogs.length < 1 || value.catalogs.length > 32) fail();
    return structuredClone(value.catalogs);
  } catch { fail(); }
  finally { if (fd !== undefined) closeSync(fd); }
}
