#!/usr/bin/env python3
"""Package the validated source reference. Requires Python 3.11+ and a passing check report."""
from pathlib import Path
import argparse,hashlib,json,zipfile
R=Path(__file__).resolve().parents[1]
GENERATED={'release.json','file-manifest.json','SHA256SUMS'}
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def inventory():
 return sorted(p for p in R.rglob('*') if p.is_file() and '__pycache__' not in p.parts and p.suffix not in {'.pyc','.pyo'})
def main():
 ap=argparse.ArgumentParser();ap.add_argument('output',type=Path);args=ap.parse_args();out=args.output.resolve()
 if out.is_relative_to(R):raise SystemExit('Output must be outside the reference directory.')
 checks=json.loads((R/'evidence/checks.json').read_text())
 if not checks.get('all_passed'):raise SystemExit('Refusing release: recorded checks are not all passing.')
 manifest=json.loads((R/'source/manifest.json').read_text())
 req=json.loads((R/'tests/requirements.json').read_text());sc=json.loads((R/'tests/scenarios.json').read_text())
 normative=[p for p in inventory() if p.parts[len(R.parts)] in {'source','api','profiles','grammar'}]
 rel={'version':'1.1.0','publication_revision':'final-2026-10-05','status':'Final consolidated source specification and implementation plan; not a binary engine release',
      'repository_writer_format':3,'coordinates':json.loads((R/'profiles/coordinates.json').read_text()),
      'counts':{'chapters':len(manifest),'requirements':len(req),'scenario_oracles':sc['count']},
      'entrypoints':{'overview':'START-HERE.md','plan':'PLAN.md','examples':'WALKTHROUGH.md','complete_reference':'Orna-1.1.0.md','html':'index.html','orchestration':'implementation/ORCHESTRATOR.md','migration':'MIGRATION.md','evidence':'evidence/VALIDATION.md'},
      'previous_draft':{'path':'compatibility/previous-1.1.0-draft.zip','sha256':digest(R/'compatibility/previous-1.1.0-draft.zip'),'authority':'Historical read/migration input only; never current writer authority'},
      'evidence':{'check_groups':checks['check_groups'],'all_passed':True,'engine_execution':'not-executed','fuse_execution':'not-executed','large_scale_execution':'not-executed'},
      'normative_payload_sha256':{str(p.relative_to(R)):digest(p) for p in normative}}
 (R/'release.json').write_text(json.dumps(rel,indent=2)+'\n')
 files=[p for p in inventory() if p.name not in {'file-manifest.json','SHA256SUMS'}]
 (R/'file-manifest.json').write_text(json.dumps({'version':'1.1.0','excludes':['file-manifest.json','SHA256SUMS'],'files':[{'path':str(p.relative_to(R)),'bytes':p.stat().st_size,'sha256':digest(p)} for p in files]},indent=2)+'\n')
 (R/'SHA256SUMS').write_text(''.join(digest(p)+'  '+str(p.relative_to(R))+'\n' for p in inventory() if p.name!='SHA256SUMS'))
 out.parent.mkdir(parents=True,exist_ok=True)
 with zipfile.ZipFile(out,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=9) as z:
  for p in inventory():
   zi=zipfile.ZipInfo('Orna-1.1.0/'+p.relative_to(R).as_posix(),date_time=(2026,10,5,0,0,0))
   zi.compress_type=zipfile.ZIP_DEFLATED;zi.create_system=3;zi.external_attr=0o100644<<16
   z.writestr(zi,p.read_bytes())
 checksum=digest(out);out.with_name(out.name+'.sha256').write_text(checksum+'  '+out.name+'\n')
 print(json.dumps({'zip':str(out),'sha256':checksum,'bytes':out.stat().st_size,'files':len(inventory()),'counts':rel['counts']},indent=2))
if __name__=='__main__':main()
