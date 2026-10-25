"""Closed baseline Compose subset: no indirect files, tags, aliases or flow maps.
This is a refusal filter, not a YAML parser. Unsupported syntax must be reviewed
rather than allowing Docker to resolve unknown includes/env_file dependencies.
"""
import hashlib,json,re
PERSISTENT_SHA='616c8b0a4dae7182177b393750193b19ba9648bf5d6029f7593c937f41da93a4'
APP_DIR='/home/deploy/jaredlikes-com'
PERSISTENT_BASE=APP_DIR+'/deployment-baselines/'+PERSISTENT_SHA+'/compose.candidate.json'
VERIFIER_PATH='/home/deploy/.config/jaredlikes-com/personal-oauth-verifier.env'

def persistent_chain(paths):
 """Same fixed persistent baseline/override namespace as canonical issuer adopter.
 No filesystem or Docker access. Legacy chains retain their old validator.
 """
 return (type(paths)is list and 1<=len(paths)<=8 and len(paths)==len(set(paths))
         and paths[0]==PERSISTENT_BASE and all(type(p)is str and re.fullmatch(
             re.escape(APP_DIR)+r'/deployment-baselines/issuer-configurations/[0-9]+-[0-9]+/production-digest-promotion\.override\.yml',p)
             for p in paths[1:]))

def checked_compose(path,raw,persistent=False):
 if persistent and path==PERSISTENT_BASE:
  if hashlib.sha256(raw).hexdigest()!=PERSISTENT_SHA:raise Refusal('persistent_baseline_drift')
  # Exact reviewed content-addressed baseline, as in issuer/activation.py.
  # No arbitrary JSON/indirect file is admitted by a .json suffix.
  return True
 if persistent:
  if not persistent_chain([PERSISTENT_BASE,path]):raise Refusal('persistent_chain_refused')
  if raw.lstrip().startswith(b'{'):
   def unique(items):
    result={}
    for key,value in items:
     if key in result:raise Refusal('compose_dependency_refused')
     result[key]=value
    return result
   try:value=json.loads(raw,object_pairs_hook=unique)
   except (ValueError,UnicodeError):raise Refusal('compose_dependency_refused')from None
   if (type(value)is not dict or set(value)!={'services'}or type(value['services'])is not dict
       or set(value['services'])!={'app'}or type(value['services']['app'])is not dict):raise Refusal('compose_dependency_refused')
   app=value['services']['app']
   if not set(app)<={'image','environment','env_file'}:raise Refusal('compose_dependency_refused')
   if 'image'in app and not re.fullmatch(r'ghcr\.io/likesjx/jaredlikes-com@sha256:[a-f0-9]{64}',str(app['image'])):raise Refusal('compose_dependency_refused')
   if 'env_file'in app and app['env_file']!=[VERIFIER_PATH]:raise Refusal('compose_dependency_refused')
   if 'environment'in app and(type(app['environment'])is not dict or any(type(k)is not str or type(v)not in(str,int,bool,type(None))for k,v in app['environment'].items())):raise Refusal('compose_dependency_refused')
   return True
 return baseline_compose(raw)
class Refusal(ValueError):pass
BANNED={'include','extends','env_file','label_file','configs','secrets','build','develop'}
def baseline_compose(raw):
 try:text=raw.decode('utf-8')
 except UnicodeError:raise Refusal('compose_dependency_refused')from None
 if len(raw)>1048576 or '\x00'in text or '\t'in text:raise Refusal('compose_dependency_refused')
 for line in text.splitlines():
  stripped=line.strip()
  if not stripped or stripped.startswith('#')or stripped in('---','...'):continue
  # Quoted scalars may contain punctuation; no YAML block scalars or alias/tag syntax.
  outside=[];i=0;quote=None
  while i<len(line):
   ch=line[i]
   if quote:
    if quote=='"'and ch=='\\':i+=2;continue
    if quote=="'"and ch=="'"and i+1<len(line)and line[i+1]=="'":i+=2;continue
    if ch==quote:quote=None
    i+=1;continue
   if ch in("'",'"'):quote=ch;i+=1;continue
   if ch=='#'and(i==0 or line[i-1].isspace()):break
   if ch=='$'and i+1<len(line)and line[i+1]=='{':
    m=re.match(r'\$\{[A-Za-z_][A-Za-z0-9_]*\}',line[i:])
    if not m:raise Refusal('compose_dependency_refused')
    outside.append('INTERPOLATION');i+=len(m[0]);continue
   outside.append(ch);i+=1
  if quote or any(c in ''.join(outside)for c in('{}[]&*!|>')):raise Refusal('compose_dependency_refused')
  value=line.lstrip();value=value[2:].lstrip()if value.startswith('- ')else value
  m=re.match(r'''(?P<key>[A-Za-z0-9_.-]+|"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')\s*:(?:\s|$)''',value)
  if m:
   key=m['key']
   if key.startswith('"'):
    try:key=json.loads(key)
    except ValueError:raise Refusal('compose_dependency_refused')from None
   elif key.startswith("'"):key=key[1:-1].replace("''", "'")
   if not re.fullmatch('[A-Za-z0-9_.-]+',key)or key in BANNED:raise Refusal('compose_dependency_refused')
  elif not stripped.startswith('- '):raise Refusal('compose_dependency_refused')
 return True
