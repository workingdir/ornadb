#!/usr/bin/env python3
"""Author deterministic format-3 golden cases; explicit regeneration, not a test."""
from pathlib import Path
import hashlib,json
from final_model import Tag,Store,store_blob,annotation,encode,component,selected_suffix
R=Path(__file__).resolve().parents[1]
def main():
 s=Store();data=b'abc';ref=store_blob(s,data,'Text/Plain; Charset="UTF-8"')
 values=[
  ('ovb2-empty',Tag(60110,[b'','application/octet-stream',None])),
  ('ovb2-text',Tag(60110,[data,'text/plain;charset=utf-8',None])),
  ('sov3-text',Tag(60112,[len(data),hashlib.sha256(data).digest(),'text/plain;charset=utf-8',None])),
  ('rov3-text',ref),
  ('ovb2-js-module',Tag(60110,[b'export {};','text/javascript','mjs']))]
 # Descriptions carry exact canonical data in a JSON-friendly typed form.
 def typed(v):
  if isinstance(v,bytes):return {'bytes_hex':v.hex()}
  if isinstance(v,Tag):return {'tag':v.number,'value':typed(v.value)}
  if isinstance(v,list):return [typed(x) for x in v]
  return v
 report={'version':'1.1.0','publication_revision':'final-2026-10-05','scope':'Golden canonical bytes from bounded reference model; not Orna production evidence.','values':[{'id':n,'value':typed(v),'canonical_hex':encode(v).hex()} for n,v in values],
   'native_graph':{'algorithm':'sha1','blob_length':3,'blob_sha256':hashlib.sha256(data).hexdigest(),'descriptor_oid':ref.value[-1].hex(),'objects':[{'oid':oid,'kind':k,'bytes_hex':b.hex()} for oid,(k,b) in sorted(s.objects.items())]},
   'paths':[{'input':x,'component':component(x)} for x in ['', '.', '..', 'london','a/b','%64ata','data','~empty','é','e\u0301','x'*300]],
   'media':[{'input':m,'hint':h,'canonical_type':annotation(m,h)[0],'canonical_hint':annotation(m,h)[1],'suffix':selected_suffix(m,h)} for m,h in [('audio/mpeg',None),('audio/mpeg','mp2'),('application/javascript','mjs'),('IMAGE/JPEG','JPEG'),('application/gzip',None),('application/gzip','tar.gz'),('application/x-example','xyz')]]}
 (R/'tests/final-vectors.json').write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
 print('Authored',len(values),'codec vectors and',len(report['native_graph']['objects']),'native objects.')
if __name__=='__main__':main()
