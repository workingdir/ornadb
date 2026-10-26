#!/usr/bin/env python3
"""Verify ZIP integrity, safe paths, complete inventories and normative payload digests."""
from pathlib import Path,PurePosixPath
import argparse,hashlib,json,zipfile
PREFIX='Orna-1.1.0/'
def main():
 ap=argparse.ArgumentParser();ap.add_argument('archive',type=Path);a=ap.parse_args();count=0
 with zipfile.ZipFile(a.archive) as z:
  if z.testzip():raise SystemExit('ZIP CRC error')
  names=z.namelist()
  if len(names)!=len(set(names)):raise SystemExit('Duplicate archive entry')
  for n in names:
   p=PurePosixPath(n)
   if not n.startswith(PREFIX) or p.is_absolute() or '..' in p.parts or '\\' in n:raise SystemExit('Unsafe path '+n)
   if (z.getinfo(n).external_attr>>16)&0o170000 != 0o100000:raise SystemExit('Non-regular entry '+n)
  def read(p):return z.read(PREFIX+p)
  sums={}
  for line in read('SHA256SUMS').decode().splitlines():
   h,p=line.split('  ',1)
   if p in sums:raise SystemExit('Duplicate checksum path')
   sums[p]=h
   if hashlib.sha256(read(p)).hexdigest()!=h:raise SystemExit('Digest mismatch '+p)
   count+=1
  present={n[len(PREFIX):] for n in names}
  if set(sums)!=present-{'SHA256SUMS'}:raise SystemExit('Checksum coverage mismatch')
  manifest=json.loads(read('file-manifest.json'))
  indexed={f['path'] for f in manifest['files']}
  if indexed!=present-{'SHA256SUMS','file-manifest.json'}:raise SystemExit('Manifest coverage mismatch')
  for f in manifest['files']:
   b=read(f['path'])
   if len(b)!=f['bytes'] or hashlib.sha256(b).hexdigest()!=f['sha256']:raise SystemExit('Manifest mismatch '+f['path'])
  release=json.loads(read('release.json'))
  for p,h in release['normative_payload_sha256'].items():
   if hashlib.sha256(read(p)).hexdigest()!=h:raise SystemExit('Normative payload mismatch '+p)
  if not json.loads(read('evidence/checks.json'))['all_passed']:raise SystemExit('Failed publication checks')
 checksum=hashlib.sha256(a.archive.read_bytes()).hexdigest()
 c=a.archive.with_name(a.archive.name+'.sha256')
 if c.exists() and c.read_text().split()[0]!=checksum:raise SystemExit('Outer ZIP digest mismatch')
 print(json.dumps({'verified':True,'entries':len(names),'checksummed_payloads':count,'sha256':checksum},indent=2))
if __name__=='__main__':main()
