#!/usr/bin/env python3
"""Static consistency validator. No compiler/runtime certification."""
from pathlib import Path
from urllib.parse import urlsplit,unquote
import json,re,hashlib,zipfile
from bs4 import BeautifulSoup
from jsonschema import Draft202012Validator
R=Path(__file__).resolve().parents[1]
def load(p):return json.loads((R/p).read_text())
def main():
 checks=[]
 def ck(name,ok,detail=None):checks.append({'name':name,'passed':bool(ok),'detail':detail})
 for p in R.rglob('*.json'):
  try:json.loads(p.read_text());ok=True
  except Exception:ok=False
  ck('json '+str(p.relative_to(R)),ok)
 m=load('source/manifest.json');req=load('tests/requirements.json');ids={r['id'] for r in req}
 ck('requirement uniqueness',len(ids)==len(req))
 ck('one repository writer',load('profiles/coordinates.json')['repository_format']==3)
 for c in m:ck('chapter '+c['id'],(R/c['file']).is_file() and '{#'+c['id']+'}' in (R/c['file']).read_text())
 a=load('api/sys.json')
 for k,n in a['counts'].items():ck('API count '+k,len(a[k])==n)
 funcs=[f['name'] for f in a['functions']];ck('unique API functions',len(funcs)==len(set(funcs)))
 ck('no placement functions',not {'sys.admin.set_storage_preference','sys.admin.rewrite_storage'}&set(funcs))
 ck('one StorageProfile',a['enums']['sys.StorageProfile']==['indexed'])
 ck('annotation APIs',all('sys.blob.'+n in funcs for n in ['media_type','suffix','annotate','same_content']))
 def basename(name):return name.split('<',1)[0].split('(',1)[0]
 known={basename(x) for x in a['opaque_identifiers']}|{basename(x) for x in funcs}
 types={}
 for bucket in ['singletons','reference_aliases','value_types','enums','relations']:
  val=a[bucket]
  if isinstance(val,dict):known.update(val)
  else:
   for x in val:
    if isinstance(x,dict):known.add(basename(x['name']));types[basename(x['name'])]=x
 for name,variants in a['enums'].items():known.update(name+'.'+v for v in variants)
 for single in a['singletons']:
  t=types.get(basename(single['type']),{})
  known.update(single['name']+'.'+f['name'] for f in t.get('fields',[]))
 refs=set()
 for f in a['functions']:refs.update(re.findall(r'\bsys(?:\.[A-Za-z_][A-Za-z0-9_]*)+',f['signature']))
 for bucket in ['value_types','relations']:
  for t in a[bucket]:
   for f in t.get('fields',[]):refs.update(re.findall(r'\bsys(?:\.[A-Za-z_][A-Za-z0-9_]*)+',f['type']))
 unknown=refs-known
 ck('API signature and field type closure',not unknown,sorted(unknown))
 for p in (R/'profiles').glob('*.schema.json'):
  try:Draft202012Validator.check_schema(json.loads(p.read_text()));ok=True
  except Exception:ok=False
  ck('schema '+p.name,ok)
 header=Draft202012Validator(load('profiles/database.schema.json'))
 ck('header accepts only format3',header.is_valid({'repository_format':3,'database_id':'9f237b7e-6844-4498-bcd5-d24641c07449'}) and not header.is_valid({'repository_format':2,'database_id':'9f237b7e-6844-4498-bcd5-d24641c07449'}))
 ck('header rejects extra key',not header.is_valid({'repository_format':3,'database_id':'9f237b7e-6844-4498-bcd5-d24641c07449','branch':'main'}))
 ev=load('tests/requirement-evidence.json')['requirements'];ck('engine evidence honest',len(ev)==len(req) and all(e['implementation_result']=='not-executed' and not e['full_implementation_coverage_claimed'] and len(e.get('tests',[]))>0 and all(t['status']=='not-executed' for t in e['tests']) for e in ev))
 sc=load('tests/scenarios.json');ck('scenario count',sc['count']==len(sc['scenarios']))
 ck('scenario uniqueness',len({x['id'] for x in sc['scenarios']})==len(sc['scenarios']))
 bad={r for x in sc['scenarios'] for r in x.get('requirements',[]) if r not in ids};ck('scenario requirement closure',not bad,sorted(bad))
 # Only current active profiles allowed; historical compatibility archive is explicit.
 ck('no old profile files',not any((R/'profiles'/x).exists() for x in ['compact-storage-v2.json','hybrid-and-rekey.md','repository-format-2.md']))
 with zipfile.ZipFile(R/'compatibility/previous-1.1.0-draft.zip') as z:
  ck('legacy wire registry unchanged',z.read('Orna-1.1.0/profiles/live-messages.json')==(R/'profiles/live-messages.json').read_bytes())
  ck('legacy grammar unchanged',z.read('Orna-1.1.0/grammar/orna.ebnf')==(R/'grammar/orna.ebnf').read_bytes())
  ck('legacy numeric vectors unchanged',z.read('Orna-1.1.0/tests/numeric-vectors.json')==(R/'tests/numeric-vectors.json').read_bytes())
 for p in (R/'tools').glob('*.py'):
  try:compile(p.read_text(),str(p),'exec');ok=True
  except SyntaxError:ok=False
  ck('python syntax '+p.name,ok)
 html=BeautifulSoup((R/'index.html').read_text(),'html.parser');anchorlist=[t['id'] for t in html.find_all(id=True)];anchors=set(anchorlist)
 ck('HTML IDs unique',len(anchorlist)==len(anchors))
 missing=[]
 for node in html.find_all(['a','link','img','script']):
  v=node.get('href') or node.get('src')
  if not v:continue
  u=urlsplit(v)
  if u.scheme or u.netloc:continue
  if not u.path:
   if u.fragment and unquote(u.fragment) not in anchors:missing.append(v)
  elif not (R/unquote(u.path)).exists():missing.append(v)
 ck('HTML local links',not missing,sorted(set(missing)))
 # Markdown local links in entrypoints + profile docs; chapter links are rendered above.
 missing=[]
 for p in list(R.glob('*.md'))+list((R/'profiles').glob('*.md'))+list((R/'implementation').glob('*.md')):
  text=re.sub(r'```[\s\S]*?```','',p.read_text())
  for v in re.findall(r'\]\(([^\s)]+)\)',text):
   u=urlsplit(v)
   if u.scheme or u.netloc or not u.path:continue
   target=unquote(u.path)
   if not (p.parent/target).exists() and not (R/target).exists():missing.append(str(p.relative_to(R))+':'+v)
 ck('entry/profile local links',not missing,sorted(set(missing)))
 goal=(R/'implementation/ORCHESTRATOR.md').read_text()
 ck('orchestrator exact models', 'openai-codex/gpt-5.6-terra' in goal and 'openai-codex/gpt-5.6-luna' in goal and 'maximum descendant depth 3' in goal)
 ck('no required MIME registry in .orna',set(load('profiles/database.schema.json')['properties'])=={'repository_format','database_id'})
 report={'scope':'Publication consistency only','passed':all(c['passed'] for c in checks),'checks':len(checks),'results':checks,'errors':[c for c in checks if not c['passed']]}
 (R/'evidence/static-results.json').write_text(json.dumps(report,indent=2)+'\n')
 print(json.dumps({'passed':report['passed'],'checks':len(checks),'errors':report['errors']},indent=2))
 return 0 if report['passed'] else 1
if __name__=='__main__':raise SystemExit(main())
