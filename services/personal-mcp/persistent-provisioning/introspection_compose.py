"""Closed baseline Compose subset: no indirect files, tags, aliases or flow maps.
This is a refusal filter, not a YAML parser. Unsupported syntax must be reviewed
rather than allowing Docker to resolve unknown includes/env_file dependencies.
"""
import json,re
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
