#!/usr/bin/env python3
"""Run scoped reference checks and retain each result before the next group starts."""
from pathlib import Path
import argparse,hashlib,json,os,platform,subprocess,sys
R=Path(__file__).resolve().parents[1]
GROUPS=[
 ('current-format-model','model_checks.py','MIME/path/CBOR/CAS/graph model and isolated native Git; not engine/FUSE'),
 ('language-syntax','syntax_probe.py','Bounded EBNF syntax probe, not compiler/type/effect checking'),
 ('legacy-float-vectors','check_float_vectors.py','Retained legacy numeric model only'),
 ('legacy-path-vectors','check_path_vectors.py','Legacy path reader compatibility, not VFS-1'),
 ('legacy-live-model','protocol_model.py','Retained wire models, not live server interoperability'),
 ('static-publication','validate.py','Cross-file source/API/profile/publication consistency')]
def fingerprint():
 h=hashlib.sha256()
 for root in ['source','api','profiles','grammar','tests','tools','implementation','examples']:
  for p in sorted((R/root).rglob('*')):
   if p.is_file() and '__pycache__' not in p.parts and p.suffix not in {'.pyc','.pyo'}:
    h.update(str(p.relative_to(R)).encode()+b'\0'+hashlib.sha256(p.read_bytes()).digest())
 for p in sorted(R.glob('*.md')):h.update(p.name.encode()+b'\0'+hashlib.sha256(p.read_bytes()).digest())
 return h.hexdigest()
def summarize(ledger,fp):
 rows=[ledger[n] for n,_,_ in GROUPS if n in ledger and ledger[n].get('input_sha256')==fp]
 report={'publication_version':'1.1.0','publication_revision':'final-2026-10-05','all_passed':len(rows)==len(GROUPS) and all(r['passed'] for r in rows),'check_groups':len(GROUPS),'completed_groups':len(rows),'input_sha256':fp,'engine_execution':'not-executed','fuse_execution':'not-executed','large_scale_execution':'not-executed','tool_versions':{'python':platform.python_version(),'git':subprocess.check_output(['git','--version'],text=True).strip()},'results':rows}
 (R/'evidence/checks.json').write_text(json.dumps(report,indent=2)+'\n')
 model=json.loads((R/'evidence/model-results.json').read_text()) if (R/'evidence/model-results.json').exists() else {}
 stats=json.loads((R/'evidence/static-results.json').read_text()) if (R/'evidence/static-results.json').exists() else {}
 text='# Executed publication checks\n\nPublication: **Orna 1.1.0 / final-2026-10-05**.\n\n'
 text+='| Group | Result | Scope |\n|---|---|---|\n'+''.join('| '+r['name']+' | '+('PASS' if r['passed'] else 'FAIL')+' | '+r['scope']+' |\n' for r in rows)
 text+=f'\nCurrent reference-model assertions: {model.get("checks","unavailable")}. Static consistency checks: {stats.get("checks","unavailable")}. Full commands/exit codes are in [checks.json](checks.json).\n'
 text+='\n## What this does not prove\n\nNo Orna engine build, real FUSE mount, editor compatibility, durable crash recovery, ten-GB import, million-row performance, independent full typed extractor or browser runtime was executed here. Those are explicitly required in ../implementation/ACCEPTANCE.md. The native Git test uses two MiB of synthetic content and places a model map at the store path to test reachability/transport; it is not a complete typed database admission proof. The local CAS model is not SQLite or a FUSE callback implementation. Legacy vectors retain their original context and do not validate the new writer.\n'
 text+='\nThe exact previous draft is retained for migration and golden comparisons. Its old evidence is not republished as new results. Re-running checks can change logs/tool versions and therefore the final artifact checksum. Each completed group is recorded immediately; --only GROUP can resume a long validation run, but groups count together only when their complete input fingerprint matches.\n'
 (R/'evidence/VALIDATION.md').write_text(text)
 return report

def main():
 ap=argparse.ArgumentParser();ap.add_argument('--only',action='append',choices=[n for n,_,_ in GROUPS]);a=ap.parse_args()
 (R/'evidence').mkdir(exist_ok=True)
 if not (R/'evidence/VALIDATION.md').exists():(R/'evidence/VALIDATION.md').write_text('# Validation\n\nChecks have not completed.\n')
 subprocess.run([sys.executable,'tools/build_publication.py'],cwd=R,check=True)
 fp=fingerprint();path=R/'evidence/group-results.json';ledger=json.loads(path.read_text()) if path.exists() else {}
 env=os.environ.copy();env['PYTHONDONTWRITEBYTECODE']='1'
 for name,script,scope in GROUPS:
  if a.only and name not in a.only:continue
  log='evidence/'+name+'.log'
  with (R/log).open('w') as out:
   try:rc=subprocess.run([sys.executable,'tools/'+script],cwd=R,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=120).returncode
   except subprocess.TimeoutExpired:rc=124;out.write('Timeout is a failure, not a pass.\n')
  ledger[name]={'name':name,'command':'python3 tools/'+script,'exit_code':rc,'passed':rc==0,'scope':scope,'log':log,'input_sha256':fp}
  path.write_text(json.dumps(ledger,indent=2)+'\n');summarize(ledger,fp)
  print(('PASS' if rc==0 else 'FAIL')+' '+name,flush=True)
 report=summarize(ledger,fp)
 if a.only:return 0 if all(ledger[n]['passed'] for n in a.only) else 1
 return 0 if report['all_passed'] else 1
if __name__=='__main__':raise SystemExit(main())
