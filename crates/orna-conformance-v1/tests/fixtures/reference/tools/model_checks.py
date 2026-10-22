#!/usr/bin/env python3
"""Execute reference policy and native-Git tests. No Orna/FUSE conformance claim."""
from pathlib import Path
import hashlib,json,os,subprocess,tempfile
from final_model import *
R=Path(__file__).resolve().parents[1]
results=[]
def check(name,condition,detail=None):
    results.append({'name':name,'passed':bool(condition),'detail':detail})
    if not condition:raise AssertionError(name)
def rejects(name,fn):
    try:fn()
    except (ValueError,TypeError,UnicodeError):check(name,True);return
    check(name,False)

def main():
    checks={
      'Text/Plain; Charset="UTF-8"':'text/plain;charset=utf-8',
      'application/javascript':'text/javascript',
      'text/plain;z="Mixed Case";a=X':'text/plain;a=X;z="Mixed Case"',
      'application/example;q="a;B"':'application/example;q="a;B"',
      'application/example;x="a\\\"b"':'application/example;x="a\\\"b"',
      'audio/x-wav':'audio/wav','application/example;x=""':'application/example;x=""'}
    for inp,out in checks.items():check('MIME '+inp,normalize_media(inp)==out)
    for inp in ['', 'text', 'text/', '/plain','text/*','*/plain','text/plain\r\nx:x','text/plain\0','text/pläin','text/plain;a=1;A=2','text/plain; a', 'text/plain; x="unterminated','text/plain;a=1;', 'text/plain;a =1','a/b\t','a/b;x=1 2']:
        rejects('reject MIME '+repr(inp),lambda inp=inp:normalize_media(inp))
    for m,e in MEDIA['entries'].items():
        check('preferred '+m,selected_suffix(m)==e['preferred'])
        for h in e['compatible']:check('hint '+m+' '+h,selected_suffix(m,h)==h)
    for bad in ['../mp3','.mp3','a/b','a\\b','a..b','a\nb','a b','a'*33,'']:
        rejects('reject suffix '+repr(bad),lambda bad=bad:annotation('application/octet-stream',bad))
    rejects('MIME/suffix incompatibility',lambda:annotation('image/jpeg','mp3'))
    check('default hint canonical',annotation('IMAGE/JPEG','JPG')==('image/jpeg',None))
    check('module hint retained',annotation('text/javascript','mjs')==('text/javascript','mjs'))
    check('gzip not tar default',selected_suffix('application/gzip')=='gz')
    check('unknown preserved',selected_suffix('application/x-new-thing','thing')=='thing')
    check('empty key',component('')=='~empty')
    check('slash encoding',component('a/b')=='a%2Fb')
    check('literal empty alias distinct',component('~empty')!='~empty')
    check('case preserved',component('A')!=component('a'))
    check('unicode distinct',component('é')!=component('e\u0301'))
    check('blob/document collision',field_name('data','application/octet-stream','orna')=='%64ata.orna')
    check('literal escape cannot alias',field_name('%64ata','application/octet-stream','orna')!='%64ata.orna')
    check('long key deterministic',component('x'*300)==component('x'*300) and component('x'*300).startswith('~key-'))
    for x in [None,False,True,0,-1,24,256,2**80,'test',b'bytes',[1,2,3],{'a':1,'b':2},Tag(60110,[b'xyz','application/octet-stream',None])]:
        check('CBOR roundtrip '+str(type(x).__name__)+str(len(results)),decode(encode(x))==x)
    for b in [b'\x18\x00',b'\x9f\xff',b'\x00\x00',b'\x58\x01x',b'\xa2aa\x01aa\x02']:
        rejects('reject CBOR '+b.hex(),lambda b=b:decode(b))
    vectors=json.loads((R/'tests/final-vectors.json').read_text())
    def untyped(v):
        if isinstance(v,dict) and 'bytes_hex' in v:return bytes.fromhex(v['bytes_hex'])
        if isinstance(v,dict) and 'tag' in v:return Tag(v['tag'],untyped(v['value']))
        if isinstance(v,list):return [untyped(x) for x in v]
        return v
    for case in vectors['values']:
        v=untyped(case['value']);b=bytes.fromhex(case['canonical_hex'])
        check('golden codec '+case['id'],encode(v)==b and decode(b)==v)
    for case in vectors['paths']:check('golden path '+case['input'][:20],component(case['input'])==case['component'])
    for case in vectors['media']:
        check('golden MIME '+case['input'],annotation(case['input'],case['hint'])==(case['canonical_type'],case['canonical_hint']) and selected_suffix(case['input'],case['hint'])==case['suffix'])
    for case in vectors['native_graph']['objects']:
        check('golden native object '+case['oid'][:9],object_id(case['kind'],bytes.fromhex(case['bytes_hex']))==case['oid'])
    payload=hashlib.shake_256(b'Orna final publication model fixture').digest(2*1024*1024)
    for alg in ('sha1','sha256'):
        s=Store(alg);ref=store_blob(s,payload,'audio/mpeg')
        chunks0={oid for oid,(k,b) in s.objects.items() if k=='blob' and b in list(chunks(payload))}
        before=set(s.objects)
        r2=store_blob(s,payload,'application/octet-stream','dat')
        check(alg+' annotations share graph',ref.value[-1]==r2.value[-1] and set(s.objects)==before)
        s.reads.clear();s.inspect_node(ref.value[-1].hex())
        check(alg+' descriptor does not fetch payload',not chunks0.intersection(s.reads))
        s.reads.clear();part=content_range(s,ref,150000,131072)
        check(alg+' range bytes',part==payload[150000:281072])
        used=chunks0.intersection(s.reads)
        check(alg+' range partial',0<len(used)<len(chunks0),{'fetched_chunks':len(used),'total_chunks':len(chunks0),'payload_bytes':sum(len(s.objects[x][1]) for x in used)})
        check(alg+' full reconstruction',content_range(s,ref,0,len(payload))==payload)
        for off,n in [(-1,1),(len(payload)+1,1),(0,8388609)]:rejects(alg+' range reject '+str((off,n)),lambda off=off,n=n:content_range(s,ref,off,n))
        check(alg+' exact EOF',content_range(s,ref,len(payload),1)==b'')
        empty=store_blob(s,b'');check(alg+' empty bytes',content_range(s,empty,0,0)==b'')
        rows=[[i,{'title':f'row {i}','content':ref}] for i in range(700)]
        root=ordered_map(s,['rows',bytes(16),bytes(32)],rows)
        check(alg+' indexed lookup',lookup(s,root,311)['title']=='row 311')
        check(alg+' absent lookup',lookup(s,root,900) is None)
        s.reads.clear();_ = lookup(s,root,311)['title'];check(alg+' row metadata no payload',not chunks0.intersection(s.reads))
        oldnodes=set(s.objects);changed=[list(e) for e in rows];changed[311]=[311,{'title':'changed','content':ref}]
        root2=ordered_map(s,['rows',bytes(16),bytes(32)],changed)
        check(alg+' replacement root',root!=root2 and lookup(s,root,311)['title']=='row 311' and lookup(s,root2,311)['title']=='changed')
        check(alg+' update payload stable',chunks0<=(set(s.objects)&oldnodes))
        rejects(alg+' duplicate keys',lambda:ordered_map(s,['test'],[[1,2],[1,3]]))
        # Wrong declared edges cannot be hidden inside an otherwise valid native tree.
        malformed=s.node([1,5,len(payload),hashlib.sha256(payload).digest(),ref.value[-1]],{})
        rejects(alg+' missing native dependency',lambda:s.inspect_node(malformed))
    e=EditModel({'title':'old'})
    rejects('invalid edit',lambda:e.accept(0,'bad',{'title':7}))
    check('invalid edit no mutation',e.value['title']=='old' and e.revision==0)
    check('accept good edit',e.accept(0,'a',{'title':'new'})==1)
    check('idempotent retry',e.accept(0,'a',{'title':'new'})==1 and e.revision==1)
    rejects('request reuse mismatch',lambda:e.accept(1,'a',{'title':'other'}))
    rejects('stale baseline',lambda:e.accept(0,'b',{'title':'other'}))
    native=native_proof(payload)
    report={'scope':'Python reference policy/encoding/graph and isolated native Git; NOT Orna evaluator, FUSE, real editor, crash safety or large-scale conformance',
            'passed':all(x['passed'] for x in results),'checks':len(results),'results':results,'native_git':native,'engine_execution':'not-executed','fuse_execution':'not-executed'}
    (R/'evidence/model-results.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({k:v for k,v in report.items() if k!='results'},indent=2))


def native_proof(payload):
    with tempfile.TemporaryDirectory(prefix='orna-final-git-') as t:
        p=Path(t);src=p/'origin';mirror=p/'mirror';clone=p/'partial';src.mkdir()
        env={k:v for k,v in os.environ.items() if not k.startswith('GIT_')}
        env.update(HOME=str(p),GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL=os.devnull,GIT_AUTHOR_NAME='Orna Reference',GIT_AUTHOR_EMAIL='reference@example.invalid',GIT_COMMITTER_NAME='Orna Reference',GIT_COMMITTER_EMAIL='reference@example.invalid',GIT_AUTHOR_DATE='2026-10-05T00:00:00Z',GIT_COMMITTER_DATE='2026-10-05T00:00:00Z')
        def git(path,*args,data=None):
            q=subprocess.run(['git','-C',str(path),*args],input=data,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=45)
            if q.returncode:raise RuntimeError('git '+args[0]+': '+q.stderr.decode(errors='replace'))
            return q.stdout
        git(src,'init','-q');git(src,'config','uploadpack.allowFilter','true');git(src,'config','uploadpack.allowAnySHA1InWant','true')
        s=Store();ref=store_blob(s,payload,'audio/mpeg');rows1=ordered_map(s,['rows',bytes(16),bytes(32)],[[1,{'title':'Original','content':ref}]])
        old=set(s.objects);rows2=ordered_map(s,['rows',bytes(16),bytes(32)],[[1,{'title':'Edited title','content':ref}]])
        for oid,(kind,b) in s.objects.items():
            check('Git native object '+oid[:9],git(src,'hash-object','-w','-t',kind,'--stdin',data=b).decode().strip()==oid)
        meta=b'{ repository_format: 3, database_id: "9f237b7e-6844-4498-bcd5-d24641c07449" }\n'
        # This proof binds a map at store for transport; full kind0/schema admission
        # is intentionally left to the production engine and declared as such.
        mid=git(src,'hash-object','-w','--stdin',data=meta).decode().strip()
        source1=b'pub table Music(id: Int) { title: Str, content: Blob, }\n'
        sid=git(src,'hash-object','-w','--stdin',data=source1).decode().strip()
        parent=None;commits=[]
        for storeoid in [rows1,rows2]:
            ornatree=git(src,'mktree',data=f'100644 blob {mid}\tdatabase.orna\n040000 tree {storeoid}\tstore\n'.encode()).decode().strip()
            root=git(src,'mktree',data=f'040000 tree {ornatree}\t.orna\n100644 blob {sid}\tmain.orna\n'.encode()).decode().strip()
            args=['commit-tree',root,'-m','Reference snapshot']+(['-p',parent] if parent else [])
            parent=git(src,*args).decode().strip();commits.append(parent)
        git(src,'update-ref','refs/heads/main',parent);git(src,'symbolic-ref','HEAD','refs/heads/main')
        git(src,'fsck','--full','--no-reflogs')
        q=subprocess.run(['git','clone','--filter=blob:none','--no-checkout','--no-local',src.as_uri(),str(clone)],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=45)
        if q.returncode:raise RuntimeError(q.stderr.decode())
        def localids():return set(git(clone,'cat-file','--batch-all-objects','--batch-check=%(objectname)').decode().splitlines())
        chunkids={object_id('blob',b) for b in chunks(payload)}
        initial=localids();check('partial clone omits all chunks',not initial&chunkids)
        class GitView(Store):
            def get(self,oid,kind=None):
                self.reads.append(oid)
                actual=git(clone,'cat-file','-t',oid).decode().strip()
                if kind and actual!=kind:raise Invalid('Git kind')
                return git(clone,'cat-file',actual,oid)
        v=GitView();got=lookup(v,rows2,1)
        check('partial row query title',got['title']=='Edited title')
        afterrow=localids();check('partial row query omits all chunks',not afterrow&chunkids)
        out=content_range(v,got['content'],150000,131072)
        check('native partial range exact',out==payload[150000:281072])
        afterrange=localids();used=afterrange&chunkids
        check('native range fetch subset',0<len(used)<len(chunkids))
        q=subprocess.run(['git','clone','--mirror',src.as_uri(),str(mirror)],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=45)
        if q.returncode:raise RuntimeError(q.stderr.decode())
        # Remove the original source before full mirror reconstruction.
        import shutil
        shutil.rmtree(src)
        class MirrorView(Store):
            def get(self,oid,kind=None):
                actual=git(mirror,'cat-file','-t',oid).decode().strip()
                if kind and actual!=kind:raise Invalid('kind')
                return git(mirror,'cat-file',actual,oid)
        mv=MirrorView();a=lookup(mv,rows1,1);b=lookup(mv,rows2,1)
        check('mirror old/new metadata',a['title']=='Original' and b['title']=='Edited title')
        restored=content_range(mv,b['content'],0,len(payload));check('mirror content exact',restored==payload)
        git(mirror,'fsck','--full')
        return {'payload_bytes':len(payload),'sha256':hashlib.sha256(payload).hexdigest(),'chunks':len(chunkids),'initial_local_media_chunks':0,'after_title_read_media_chunks':0,'requested_bytes':131072,'range_media_chunks':len(used),'range_fetched_payload_bytes':sum(len(s.objects[x][1]) for x in used),'git_fsck':True,'offline_mirror_exact':True,'metadata_versions':2,'limitations':'Two-MiB reference content; map directly at store for transport proof, no full database schema admission, FUSE or fault-injection.'}
if __name__=='__main__':main()
