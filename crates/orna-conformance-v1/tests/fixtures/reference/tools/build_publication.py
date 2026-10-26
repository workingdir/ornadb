#!/usr/bin/env python3
"""Regenerate publication/indexes. Does not execute the Orna engine."""
from pathlib import Path
import hashlib,json,re,subprocess,sys,html
R=Path(__file__).resolve().parents[1]
def load(p):return json.loads((R/p).read_text())
def save(p,v):(R/p).write_text(json.dumps(v,indent=2,ensure_ascii=False)+'\n')
def main():
 subprocess.run([sys.executable,'tools/generate_api_reference.py'],cwd=R,check=True)
 manifest=load('source/manifest.json');req=[]
 for n,c in enumerate(manifest,1):
  p=R/c['file'];text=p.read_text();text=re.sub(r'^# [^\n]+',f'# {n}. {c["title"]} {{#{c["id"]}}}',text,count=1);p.write_text(text)
  for ln,line in enumerate(text.splitlines(),1):
   m=re.match(r'^\*\*(ORNA-[A-Z0-9-]+)\*\*\s+(.+)',line)
   if m:req.append({'id':m[1],'chapter':c['id'],'source':c['file'],'line':ln,'text':m[2]})
 if len({r['id'] for r in req})!=len(req):raise ValueError('duplicate requirement ID')
 save('tests/requirements.json',req)
 ids={r['id'] for r in req}
 # Read baseline from exact compatibility archive.
 import zipfile
 with zipfile.ZipFile(R/'compatibility/previous-1.1.0-draft.zip') as z:
  old=json.loads(z.read('Orna-1.1.0/tests/requirements.json'))
 oldby={r['id']:r for r in old};newby={r['id']:r for r in req}
 save('provenance/requirement-changes.json',{
  'added':[r for r in req if r['id'] not in oldby],
  'revised':[{'id':r['id'],'before':oldby[r['id']]['text'],'after':r['text']} for r in req if r['id'] in oldby and r['text']!=oldby[r['id']]['text']],
  'retired':[{'id':r['id'],'previous_source':r['source'],'reason':'Superseded by the final single-row-store/native graph/VFS contract; exact legacy requirement retained in compatibility archive.'} for r in old if r['id'] not in ids]})
 scenarios=[]
 groups=[
 ('ROW','row store',['ORNA-ROW-']),('GRAPH','native reachability',['ORNA-GRAPH-']),
 ('BLOB','annotated/lazy content',['ORNA-BLOB-','ORNA-OGB-']),
 ('MIME','canonical declaration and suffix',['ORNA-MIME-']),('VFS','filesystem editing',['ORNA-VFS-']),
 ('CAP','capture and durability',['ORNA-CAP-']),('UPGRADE','migration',['ORNA-UPGRADE-']),
 ('STAGE','index and checkout',['ORNA-STAGE-','ORNA-CHECKOUT-']),('APP','minimal source hosted app',['ORNA-APP-'])]
 for tag,title,prefixes in groups:
  for r in req:
   if any(r['id'].startswith(p) for p in prefixes):
    scenarios.append({'id':'FINAL-'+r['id'][5:],'title':title+': '+r['id'],'requirements':[r['id']],
      'given':['A disposable database using the final format and the actual production implementation.'],
      'when':['Exercise the rule, its rejected inputs and the relevant concurrency/recovery boundary.'],
      'then':[r['text']], 'implementation_status':'not-executed','gate_matrix':'implementation/ACCEPTANCE.md'})
 # Retain old scenario oracles only when all requirements still exist and no obsolete
 # row-format policy is embedded in the scenario.
 legacy=load('tests/legacy-scenarios.json');retained=[];dispositions=[]
 for s in legacy['scenarios']:
  text=json.dumps(s).lower()
  stale=re.search(r'editable|parquet|hybrid|key-path|format-2|orr-2|rov-2|storage preference|loose.row',text)
  ok=set(s.get('requirements',[]))<=ids and not stale
  if ok:
   ss=dict(s);ss['implementation_status']='not-executed';ss['origin']='retained behavioral oracle; final context applies';retained.append(ss)
  else:dispositions.append({'id':s['id'],'status':'legacy-context-only','reason':'retired requirement or superseded physical row/codec policy'})
 save('tests/final-scenarios.json',{'version':'1.1.0','scope':'Implementation test oracles, not executed proofs','count':len(scenarios),'scenarios':scenarios})
 save('tests/scenarios.json',{'version':'1.1.0','count':len(retained)+len(scenarios),'scenarios':retained+scenarios})
 save('provenance/scenario-dispositions.json',dispositions)
 allsc=retained+scenarios
 save('tests/requirement-evidence.json',{'version':'1.1.0','scope':'Every status below refers to the Orna engine, not Python reference checks.','requirements':[{'requirement':r['id'],'source':r['source'],'implementation_result':'not-executed','full_implementation_coverage_claimed':False,'scenario_oracles':[s['id'] for s in allsc if r['id'] in s.get('requirements',[])],'tests':([{'kind':'scenario','subject':s['id'],'path':'tests/scenarios.json','status':'not-executed'} for s in allsc if r['id'] in s.get('requirements',[])] or [{'kind':'implementation-suite','subject':r['id'],'path':'implementation/ACCEPTANCE.md','status':'not-executed','obligation':r['text']}])} for r in req]})
 title='''# Orna 1.1.0 {#orna-specification}

**Final consolidated source reference — publication revision final-2026-10-05.**

Conceptual integrity: one executable Git snapshot, one indexed row store,
annotated immutable Blobs, one mutation engine, and editable VFS projections.
Repository writer format 3. Legacy readers retain their own recorded profiles.

This is a specification and implementation plan, not a compiled engine release.
Reference checks do not establish production/FUSE/crash/scale conformance.

[Start here](START-HERE.md) · [Plan](PLAN.md) · [Walkthrough](WALKTHROUGH.md) ·
[Migration](MIGRATION.md) · [Goal](implementation/ORCHESTRATOR.md) ·
[Validation](evidence/VALIDATION.md)

'''
 book=title+'\n\n'.join((R/c['file']).read_text().strip() for c in manifest)+'\n'
 (R/'Orna-1.1.0.md').write_text(book)
 subprocess.run(['pandoc','Orna-1.1.0.md','--from=markdown+fenced_divs+header_attributes','--to=html5','--standalone','--toc','--toc-depth=2','--metadata=title:Orna 1.1.0 — final consolidated specification','--css=publication/spec.css','--output=index.html'],cwd=R,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 print(json.dumps({'chapters':len(manifest),'requirements':len(req),'scenarios':len(allsc),'engine_execution':'not-executed'}))
if __name__=='__main__':main()
