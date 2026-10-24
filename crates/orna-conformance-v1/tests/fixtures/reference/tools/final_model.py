#!/usr/bin/env python3
"""Small format-3 reference algorithms; NOT an Orna engine or FUSE implementation.
Only byte encoding, MIME normalization, filename projection, graph construction,
row partitioning and range reads below are executable model evidence.
"""
from __future__ import annotations
from pathlib import Path
from dataclasses import dataclass
import hashlib,json,re,struct
from typing import Any
from value_model import Tag, encode
from blob_model import tree_bytes,parse_tree,object_id
R=Path(__file__).resolve().parents[1]
MEDIA=json.loads((R/'profiles/media-1.json').read_text())
TOKEN=r"[!#$%&'*+.^_`|~0-9A-Za-z-]+"
TOKEN_RE=re.compile(TOKEN)
SUFFIX_RE=re.compile(r'[a-z0-9_-]+(?:\.[a-z0-9_-]+)*\Z')
class Invalid(ValueError): pass

def normalize_media(value:str)->str:
    if not isinstance(value,str) or len(value.encode('utf-8'))>1024:
        raise Invalid('sys.blob.invalid_media_type')
    if any(ord(c)<32 or ord(c)>126 for c in value):raise Invalid('sys.blob.invalid_media_type')
    s=value.strip(' ');p=0
    def token():
        nonlocal p
        m=TOKEN_RE.match(s,p)
        if not m:raise Invalid('sys.blob.invalid_media_type')
        p=m.end();return m[0]
    typ=token()
    if p>=len(s) or s[p]!='/':raise Invalid('sys.blob.invalid_media_type')
    p+=1;sub=token();essence=(typ+'/'+sub).lower()
    if typ=='*' or sub=='*':raise Invalid('sys.blob.invalid_media_type')
    essence=MEDIA['aliases'].get(essence,essence);params={}
    while p<len(s):
        while p<len(s) and s[p]==' ':p+=1
        if p==len(s):break
        if s[p]!=';':raise Invalid('sys.blob.invalid_media_type')
        p+=1
        while p<len(s) and s[p]==' ':p+=1
        name=token().lower()
        if p>=len(s) or s[p]!='=':raise Invalid('sys.blob.invalid_media_type')
        p+=1
        if p<len(s) and s[p]=='"':
            p+=1;parts=[];closed=False
            while p<len(s):
                c=s[p];p+=1
                if c=='"':closed=True;break
                if c=='\\':
                    if p==len(s):raise Invalid('sys.blob.invalid_media_type')
                    c=s[p];p+=1
                parts.append(c)
            if not closed:raise Invalid('sys.blob.invalid_media_type')
            val=''.join(parts)
        else:val=token()
        if name in params or len(params)>=32:raise Invalid('sys.blob.invalid_media_type')
        params[name]=val.lower() if name=='charset' else val
    def q(v):return v if re.fullmatch(TOKEN,v) else '"'+v.replace('\\','\\\\').replace('"','\\"')+'"'
    return essence+''.join(';'+n+'='+q(v) for n,v in sorted(params.items()))

def annotation(media:str='application/octet-stream',hint:str|None=None):
    media=normalize_media(media);essence=media.split(';',1)[0]
    entry=MEDIA['entries'].get(essence);preferred=entry['preferred'] if entry else 'bin'
    if hint is not None:
        if not isinstance(hint,str):raise Invalid('sys.blob.invalid_suffix')
        hint=hint.lower()
        if len(hint)>32 or not SUFFIX_RE.fullmatch(hint):raise Invalid('sys.blob.invalid_suffix')
        if entry and essence!='application/octet-stream' and hint not in entry['compatible']:
            raise Invalid('sys.blob.incompatible_suffix')
        if hint==preferred:hint=None
    return media,hint

def selected_suffix(media:str,hint:str|None=None):
    m,h=annotation(media,hint)
    return h or MEDIA['entries'].get(m.split(';')[0],{}).get('preferred','bin')

def component(text:str,reserved=()):
    """Linux VFS component spelling. Long aliases need a checked index outside this model."""
    if text=='':return '~empty'
    data=text.encode('utf8');allowed=b'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-'
    stem=text.rstrip(' .').split('.',1)[0].upper()
    devices={'CON','PRN','AUX','NUL','CLOCK$','CONIN$','CONOUT$'}|{f'{p}{i}' for p in ['COM','LPT'] for i in range(1,10)}
    special=text in {'.','..','.git','.orna',*reserved} or stem in devices
    out=''.join(chr(c) if c in allowed else f'%{c:02X}' for c in data)
    if special and data[0] in allowed:out=f'%{data[0]:02X}'+out[1:]
    if out.endswith('.'):
        trail=len(out)-len(out.rstrip('.'));out=out[:-trail]+'%2E'*trail
    if len(out.encode())>200:return '~key-'+hashlib.sha256(encode(text)).hexdigest()
    return out

def field_name(name:str,media:str,hint=None):
    out=component(name)+'.'+selected_suffix(media,hint)
    if out=='data.orna':out='%64ata.orna'
    return out

def decode(data:bytes,max_bytes=65536):
    """Strict canonical-CBOR model subset; does not prove the complete Orna type graph."""
    if len(data)>max_bytes:raise Invalid('limit')
    pos=0;budget=100000
    def take(n):
        nonlocal pos
        if n<0 or pos+n>len(data):raise Invalid('truncated')
        v=data[pos:pos+n];pos+=n;return v
    def item(depth=0):
        nonlocal budget
        budget-=1
        if depth>64 or budget<0:raise Invalid('limit')
        c=take(1)[0];major=c>>5;ai=c&31
        if major==7:
            if ai==20:return False
            if ai==21:return True
            if ai==22:return None
            if ai==27:return struct.unpack('>d',take(8))[0]
            raise Invalid('simple value')
        if ai<24:n=ai
        elif ai in (24,25,26,27):
            width={24:1,25:2,26:4,27:8}[ai];n=int.from_bytes(take(width),'big')
            if n<{24:24,25:256,26:65536,27:4294967296}[ai]:raise Invalid('noncanonical')
        else:raise Invalid('indefinite')
        if major==0:return n
        if major==1:return -1-n
        if major==2:return take(n)
        if major==3:return take(n).decode('utf8')
        if major==4:
            if n>budget:raise Invalid('limit')
            return [item(depth+1) for _ in range(n)]
        if major==5:
            if n*2>budget:raise Invalid('limit')
            result={};previous=None
            for _ in range(n):
                start=pos;k=item(depth+1);raw=data[start:pos]
                if previous is not None and raw<=previous:raise Invalid('map ordering')
                previous=raw
                if not isinstance(k,(str,bytes,int)) or isinstance(k,bool):raise Invalid('model key type')
                result[k]=item(depth+1)
            return result
        if major==6:
            v=item(depth+1)
            if n in (2,3):
                if not isinstance(v,bytes) or len(v)<9 or v[0]==0:raise Invalid('bignum')
                x=int.from_bytes(v,'big');return x if n==2 else -1-x
            if n not in {60110,60111,60112,60113} and not (37==n or 60000<=n<=60025):raise Invalid('tag')
            return Tag(n,v)
        raise Invalid('CBOR major')
    x=item()
    if pos!=len(data) or encode(x)!=data:raise Invalid('canonical/trailing')
    return x

class Store:
    def __init__(self,algorithm='sha1'):
        if algorithm not in ('sha1','sha256'):raise Invalid('Git hash')
        self.algorithm=algorithm;self.objects={};self.reads=[]
    def put(self,kind,data):
        oid=object_id(kind,data,self.algorithm)
        if oid in self.objects and self.objects[oid]!=(kind,data):raise Invalid('collision')
        self.objects[oid]=(kind,data);return oid
    def get(self,oid,kind=None):
        self.reads.append(oid)
        if oid not in self.objects:raise Invalid('unavailable')
        k,b=self.objects[oid]
        if kind is not None and k!=kind:raise Invalid('kind')
        if object_id(k,b,self.algorithm)!=oid:raise Invalid('object digest')
        return b
    def tree(self,entries):return self.put('tree',tree_bytes(entries,self.algorithm))
    def node(self,data,deps):
        if len(encode(data))>65536 or len(deps)>256:raise Invalid('node bounds')
        e={'data':('blob',self.put('blob',encode(data)))}
        if deps:e['refs']=('tree',self.tree({oid:(kind,oid) for oid,kind in deps.items()}))
        return self.tree(e)
    def inspect_node(self,oid):
        e=parse_tree(self.get(oid,'tree'),self.algorithm)
        if set(e) not in ({'data'},{'data','refs'}) or e['data'][0]!='blob':raise Invalid('node shape')
        data=decode(self.get(e['data'][1],'blob'));deps={}
        if 'refs' in e:
            if e['refs'][0]!='tree':raise Invalid('refs type')
            for name,(kind,ref) in parse_tree(self.get(e['refs'][1],'tree'),self.algorithm).items():
                if name!=ref:raise Invalid('ref name')
                deps[ref]=kind
        if len(deps)>256:raise Invalid('refs bound')
        if expected_dependencies(data)!=deps:raise Invalid('dependency mismatch')
        return data

def blob_references(v):
    result={}
    if isinstance(v,Tag):
        if v.number==60111:
            if not isinstance(v.value,list) or len(v.value)!=5:raise Invalid('blob ref')
            n,sha,mime,hint,oid=v.value
            if type(n)!=int or not 0<=n<(1<<63) or not isinstance(sha,bytes) or len(sha)!=32:raise Invalid('blob commitment')
            if (mime,hint)!=annotation(mime,hint):raise Invalid('blob annotation')
            result[oid.hex()]='tree'
        elif v.number==60113:result[v.value[0].hex()]='tree'
        else:result.update(blob_references(v.value))
    elif isinstance(v,(tuple,list)):
        for x in v:result.update(blob_references(x))
    elif isinstance(v,dict):
        for k,x in v.items():result.update(blob_references(k));result.update(blob_references(x))
    return result

def expected_dependencies(d):
    if not isinstance(d,list) or len(d)<3 or d[0]!=1:raise Invalid('node version')
    kind=d[1];deps={}
    def add(oid,t='tree'):
        if not isinstance(oid,bytes) or len(oid) not in (20,32):raise Invalid('oid')
        h=oid.hex()
        if h in deps and deps[h]!=t:raise Invalid('ref kind collision')
        deps[h]=t
    if kind==0:add(d[2])
    elif kind==1:
        deps.update(blob_references(d[3]))
        # Relation map values explicitly carry native schema/primary/secondary refs.
        if d[2][0]=='relations':
            for _,v in d[3]:
                add(v[0]);add(v[1])
                if v[2] is not None:add(v[2])
    elif kind==2:
        for key,oid,_ in d[4]:deps.update(blob_references(key));add(oid)
    elif kind==3:
        add(d[4])
        if d[5] is not None:add(d[5])
    elif kind==4:
        for n,oid,sha in d[4]:
            if type(n)!=int or n<=0:raise Invalid('span')
            add(oid,'blob' if d[2]==0 else 'tree')
            if d[2]==0 and (not isinstance(sha,bytes) or len(sha)!=32):raise Invalid('chunk hash')
            if d[2]!=0 and sha is not None:raise Invalid('nonleaf hash')
        if sum(x[0] for x in d[4])!=d[3] or not d[4]:raise Invalid('span total')
    elif kind==5:
        if len(d)!=5 or type(d[2])!=int or not 0<=d[2]<(1<<63) or len(d[3])!=32:raise Invalid('descriptor')
        if d[2]==0:
            if d[4] is not None or d[3]!=hashlib.sha256(b'').digest():raise Invalid('empty descriptor')
        else:add(d[4])
    elif kind==6:add(d[4])
    elif kind==7:
        for oid in d[3]:add(oid)
    else:raise Invalid('node kind')
    return deps

GEAR=[int.from_bytes(hashlib.sha256(b'orna.gear.v1\0'+bytes([x])).digest()[:8],'big') for x in range(256)]
def chunks(data:bytes):
    start=0;a=0
    for i,x in enumerate(data,1):
        a=((a<<1)+GEAR[x])&((1<<64)-1);size=i-start
        if size>=65536 and (a&262143==0 or size>=1048576):
            yield data[start:i];start=i;a=0
    if start<len(data):yield data[start:]

def partition(entries,tokens,level,make_data,dep_fn,domain=b'orna.orp.boundary.v1\0'):
    current=[]
    for e,token in zip(entries,tokens,strict=True):
        cand=current+[e]
        if current and (len(cand)>256 or len(encode(make_data(cand)))>65536 or len(dep_fn(cand))>256):
            yield current;current=[]
        current.append(e)
        if len(encode(make_data(current)))>65536 or len(dep_fn(current))>256:raise Invalid('overflow lowering required')
        h=hashlib.sha256(domain+level.to_bytes(4,'big')+token).digest()
        if len(current)==256 or len(current)>=16 and h[-1]&63==0:
            yield current;current=[]
    if current:yield current

def byte_index(store:Store,entries,height=0):
    if not entries:raise Invalid('empty index')
    while True:
        if height>64:raise Invalid('height')
        def form(es):return [1,4,height,sum(x[0] for x in es),es]
        def refs(es):return {x[1].hex():('blob' if height==0 else 'tree') for x in es}
        tokens=[encode([e[0],e[2]]) if height==0 else e[1] for e in entries]
        groups=list(partition(entries,tokens,height,form,refs,b'orna.ogb.index.v2\0'))
        parents=[]
        for g in groups:
            oid=store.node(form(g),refs(g));parents.append([sum(e[0] for e in g),bytes.fromhex(oid),None])
        if len(parents)==1:return parents[0][1].hex()
        entries=parents;height+=1

def store_blob(store:Store,data:bytes,media='application/octet-stream',hint=None):
    media,hint=annotation(media,hint);entries=[]
    for c in chunks(data):
        oid=store.put('blob',c);entries.append([len(c),bytes.fromhex(oid),hashlib.sha256(c).digest()])
    root=byte_index(store,entries) if entries else None
    d=[1,5,len(data),hashlib.sha256(data).digest(),bytes.fromhex(root) if root else None]
    desc=store.node(d,expected_dependencies(d))
    return Tag(60111,[len(data),hashlib.sha256(data).digest(),media,hint,bytes.fromhex(desc)])

def content_range(store,ref,offset,length):
    n,sha,media,hint,desc=ref.value
    if offset<0 or length<0 or offset>n or length>8*1024*1024:raise Invalid('range')
    d=store.inspect_node(desc.hex())
    if d[1]!=5 or d[2]!=n or d[3]!=sha:raise Invalid('reference descriptor mismatch')
    end=min(n,offset+length)
    if end<=offset:return b''
    result=[]
    def walk(oid,start,want_height=None):
        node=store.inspect_node(oid)
        if node[1]!=4 or want_height is not None and node[2]!=want_height:raise Invalid('index height')
        cursor=start
        for size,target,chunk_sha in node[4]:
            stop=cursor+size
            if stop>offset and cursor<end:
                if node[2]==0:
                    b=store.get(target.hex(),'blob')
                    if len(b)!=size or hashlib.sha256(b).digest()!=chunk_sha:raise Invalid('chunk bytes')
                    result.append(b[max(0,offset-cursor):min(size,end-cursor)])
                else:walk(target.hex(),cursor,node[2]-1)
            cursor=stop
    root=store.inspect_node(d[4].hex())
    if root[3]!=n:raise Invalid('root length')
    walk(d[4].hex(),0)
    return b''.join(result)

def ordered_map(store,domain,entries):
    """Executable key subset: homogeneous Int, Str or byte keys in Python logical order."""
    entries=sorted(entries,key=lambda x:x[0])
    if any(entries[i-1][0]==entries[i][0] for i in range(1,len(entries))):raise Invalid('duplicate key')
    if not entries:
        d=[1,1,domain,[]];return store.node(d,expected_dependencies(d))
    def build_groups(es,h):
        def form(g):return [1,1,domain,g] if h==0 else [1,2,domain,h,g]
        def dep(g):return expected_dependencies(form(g))
        return list(partition(es,[encode(e[0]) for e in es],h,form,dep)),form,dep
    h=0
    while True:
        if h>64:raise Invalid('height')
        groups,form,dep=build_groups(entries,h);parents=[]
        for g in groups:
            oid=store.node(form(g),dep(g));count=len(g) if h==0 else sum(x[2] for x in g)
            parents.append([g[-1][0],bytes.fromhex(oid),count])
        if len(parents)==1:return parents[0][1].hex()
        entries=parents;h+=1

def lookup(store,root,key):
    n=store.inspect_node(root)
    if n[1]==1:
        return next((v for k,v in n[3] if k==key),None)
    if n[1]!=2:raise Invalid('map node')
    for end,oid,count in n[4]:
        if key<=end:return lookup(store,oid.hex(),key)
    return None

@dataclass
class EditModel:
    """CAS/idempotency model only: NO filesystem, persistent transactions or crash proof."""
    value: dict
    revision:int=0
    def __post_init__(self):self.outcomes={}
    def accept(self,base:int,edit_id:str,candidate:dict):
        fingerprint=hashlib.sha256(encode(candidate)).digest()
        if edit_id in self.outcomes:
            old,result=self.outcomes[edit_id]
            if old!=fingerprint:raise Invalid('request mismatch')
            return result
        if base!=self.revision:raise Invalid('stale version')
        if not isinstance(candidate.get('title'),str):raise Invalid('type mismatch')
        self.value=dict(candidate);self.revision+=1
        self.outcomes[edit_id]=(fingerprint,self.revision);return self.revision
