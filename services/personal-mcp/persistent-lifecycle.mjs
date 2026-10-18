// Single supervised owner of lifecycle only, never issuer grants or memory data.
// Exclusive lock is deliberately not auto-reaped: crash recovery requires review.
import * as fs from 'node:fs';
import path from 'node:path';
const check = (v, m) => { if (!v) throw new Error(m); };
const integer = v => Number.isSafeInteger(v) && v >= 0;
const phases = ['withdrawn', 'active', 'rotation-pending'];
const flags = fs.constants;
export class PersistentLifecycle {
  #dir; #lock; #identity; #state; #leases = new Set(); #closed = false; #poisoned = false; #clock; #draining=false; #closing=false;
  constructor(directory, {clock = Date.now} = {}) {
    this.#dir = fs.realpathSync(directory); this.#clock = clock;
    const dir = fs.lstatSync(this.#dir);
    check(dir.isDirectory() && dir.uid === process.getuid() && (dir.mode & 0o777) === 0o700, 'private lifecycle directory required');
    this.#lock = fs.openSync(path.join(this.#dir,'owner.lock'),flags.O_CREAT|flags.O_EXCL|flags.O_WRONLY|flags.O_NOFOLLOW,0o600);
    this.#identity = fs.fstatSync(this.#lock);
    try {
      fs.writeFileSync(this.#lock, JSON.stringify({pid:process.pid})); fs.fsyncSync(this.#lock); this.#syncDirectory();
      const file=path.join(this.#dir,'state.json');
      if (fs.existsSync(file)) {
        const fd=fs.openSync(file,flags.O_RDONLY|flags.O_NOFOLLOW);
        try {
          const s=fs.fstatSync(fd);check(s.isFile()&&s.uid===process.getuid()&&(s.mode&0o777)===0o600&&s.nlink===1&&s.size<=8192,'unsafe lifecycle state');
          this.#state=JSON.parse(fs.readFileSync(fd,'utf8'));
        } finally {fs.closeSync(fd);}
        check(this.#state.schema===1&&integer(this.#state.revision)&&phases.includes(this.#state.phase),'invalid lifecycle state');
        check(this.#state.phase==='withdrawn','unclean state requires recovery');
      } else this.#state={schema:1,revision:0,phase:'withdrawn',credentialReference:null,credentialExpiresAt:0};
      this.#persist(this.#state);
    } catch (error) {
      // Keep owner.lock as a durable recovery interlock; do not guess ownership.
      this.#poisoned=true;fs.closeSync(this.#lock);this.#closed=true;throw error;
    }
  }
  #syncDirectory() {const fd=fs.openSync(this.#dir,flags.O_RDONLY);try{fs.fsyncSync(fd);}finally{fs.closeSync(fd);}}
  #usable() {check(!this.#closed&&!this.#poisoned,'lifecycle unavailable');}
  #persist(next) {
    const pending=path.join(this.#dir,'state.pending');let fd;
    try {
      fd=fs.openSync(pending,flags.O_WRONLY|flags.O_CREAT|flags.O_EXCL|flags.O_NOFOLLOW,0o600);
      fs.writeFileSync(fd,JSON.stringify(next)+'\n');fs.fsyncSync(fd);fs.closeSync(fd);fd=undefined;
      fs.renameSync(pending,path.join(this.#dir,'state.json'));this.#syncDirectory();this.#state=next;
    } catch(e) {if(fd!==undefined)fs.closeSync(fd);this.#poisoned=true;for(const lease of this.#leases)lease.abort();throw e;}
  }
  #transition(expected, patch) {
    this.#usable();check(this.#state.revision===expected&&expected<Number.MAX_SAFE_INTEGER,'stale lifecycle revision');
    this.#persist({...this.#state,...patch,revision:expected+1});return this.#state.revision;
  }
  read() {
    this.#usable();
    if(this.#state.credentialExpiresAt*1000<=this.#clock()&&this.#state.phase==='active') {
      // Persist observed expiry: a wall-clock rollback must never reopen access.
      this.#transition(this.#state.revision,{phase:'withdrawn'});
      for(const lease of [...this.#leases])lease.abort();
    }
    return structuredClone(this.#state);
  }
  activate(expected, {reference,expiresAt}) {
    this.#usable();check(!this.#draining&&!this.#closing&&this.#state.phase==='withdrawn'&&this.#leases.size===0,'not quiescent');
    check(typeof reference==='string'&&/^secret:\/\/[A-Za-z0-9/_-]{1,256}$/.test(reference)&&integer(expiresAt)&&expiresAt*1000>this.#clock(),'invalid credential lease');
    return this.#transition(expected,{phase:'active',credentialReference:reference,credentialExpiresAt:expiresAt});
  }
  acquireResponse(response) {
    check(!this.#closing&&this.read().phase==='active','serving withdrawn');
    check(!response.destroyed&&!response.writableEnded,'response already closed');
    const controller=new AbortController();let timer,finished=false;
    const lease={abort:()=>{controller.abort();response.destroy();}};
    const release=()=>{if(finished)return;finished=true;controller.abort();clearTimeout(timer);this.#leases.delete(lease);
      for(const event of ['finish','close'])response.removeListener(event,release);
      response.removeListener('error',onError);};
    // An error alone is not proof that transport closed. Destroy it and keep
    // the lease until close/finish confirms termination; drain timeout fails shut.
    const onError=()=>lease.abort();
    for(const event of ['finish','close'])response.once(event,release);
    response.once('error',onError);
    this.#leases.add(lease);
    const schedule=()=>{const remaining=this.#state.credentialExpiresAt*1000-this.#clock();
      if(remaining<=0){this.read();lease.abort();return;}timer=setTimeout(schedule,Math.min(remaining,2147483647));timer.unref?.();};
    schedule();
    return Object.freeze({signal:controller.signal,assertServing:()=>{check(!finished&&!controller.signal.aborted&&this.read().phase==='active','response withdrawn');},abort:lease.abort});
  }
  async withdraw(expected, {rotation=false,timeoutMs=5000}={}) {
    check(!this.#draining,'withdrawal already pending');
    check(integer(timeoutMs)&&timeoutMs>0&&timeoutMs<=30000,'invalid drain bound');
    const revision=this.#transition(expected,{phase:rotation?'rotation-pending':'withdrawn'});
    this.#draining=true;
    try {
    for(const lease of [...this.#leases])lease.abort();
    const deadline=Date.now()+timeoutMs;
    while(this.#leases.size) {
      check(Date.now()<deadline,'response drain incomplete');await new Promise(resolve=>setTimeout(resolve,5));
    }
    return revision;
    } finally {this.#draining=false;}
  }
  completeRotation(expected, credential, {adoptedReference,oldCredentialDenied}) {
    check(!this.#closing&&!this.#draining&&this.read().phase==='rotation-pending'&&this.#leases.size===0&&oldCredentialDenied===true&&adoptedReference===credential.reference&&
      credential.reference!==this.#state.credentialReference,'rotation evidence incomplete');
    // Remain withdrawn until the independently verified replacement is explicit.
    const revision=this.#transition(expected,{phase:'withdrawn'});
    return this.activate(revision,credential);
  }
  async close() {
    this.#usable();this.#closing=true;await this.withdraw(this.#state.revision);
    const p=path.join(this.#dir,'owner.lock'),s=fs.lstatSync(p);
    check(s.dev===this.#identity.dev&&s.ino===this.#identity.ino,'owner lock changed');
    fs.closeSync(this.#lock);fs.unlinkSync(p);this.#syncDirectory();this.#closed=true;
  }
}

// ServerResponse lifecycle fence. Lease is released on finish/close,
// never when recall's promise resolves. Withdrawal destroys pending responses.
// This does not coordinate issuer/privacy mutations and cannot retract bytes
// already handed to the transport. Production composition requires those owners.
export async function servePersistentRecall({lifecycle, response, recall}) {
  const lease=lifecycle.acquireResponse(response);
  try {
    const result=await recall({signal:lease.signal});
    const body=JSON.stringify(result);lease.assertServing();
    response.writeHead(200,{'Content-Type':'application/json','Cache-Control':'no-store'});
    response.end(body);
  } catch(e) {lease.abort();throw e;}
}
