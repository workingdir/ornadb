#!/usr/bin/env python3
"""OGB-1 reference model. Not an Orna runtime or production streaming store."""
from __future__ import annotations
from dataclasses import dataclass
import hashlib
from typing import Iterable

MAX_LENGTH=(1<<63)-1
MAX_CHUNK=1<<20
MAX_META=1<<16
MAX_READ=8<<20
FANOUT=256

class BlobError(ValueError):
    pass

@dataclass(frozen=True)
class Tag:
    number: int
    value: object

def _head(major: int, n: int) -> bytes:
    if n < 0 or n >= 1<<64: raise BlobError('integer out of CBOR subset')
    for width,info,limit in [(0,0,24),(1,24,256),(2,25,65536),(4,26,1<<32),(8,27,1<<64)]:
        if n < limit:
            return bytes([(major<<5)|(n if width==0 else info)])+(n.to_bytes(width,'big') if width else b'')
    raise BlobError('overflow')

def encode(v: object) -> bytes:
    if v is None:return b'\xf6'
    if type(v) is bool:return b'\xf5' if v else b'\xf4'
    if type(v) is int:return _head(0,v) if v>=0 else _head(1,-1-v)
    if isinstance(v,bytes):return _head(2,len(v))+v
    if isinstance(v,str):
        b=v.encode();return _head(3,len(b))+b
    if isinstance(v,(list,tuple)):return _head(4,len(v))+b''.join(encode(x) for x in v)
    if isinstance(v,Tag):return _head(6,v.number)+encode(v.value)
    raise BlobError('unsupported model value')

def decode(data: bytes, *, allow_tag: bool=False, maximum: int=MAX_META) -> object:
    if len(data)>maximum:raise BlobError('metadata limit')
    pos=0;items=0
    def read(depth=0):
        nonlocal pos,items
        items+=1
        if depth>16 or items>2000 or pos>=len(data):raise BlobError('bounded/truncated metadata')
        c=data[pos];pos+=1;major=c>>5;ai=c&31
        if major==7:
            if c==0xf6:return None
            if c in (0xf4,0xf5):return c==0xf5
            raise BlobError('unsupported simple value')
        if ai<24:n=ai
        else:
            width={24:1,25:2,26:4,27:8}.get(ai)
            if width is None or pos+width>len(data):raise BlobError('invalid CBOR head')
            n=int.from_bytes(data[pos:pos+width],'big');pos+=width
            if n < {1:24,2:256,4:65536,8:1<<32}[width]:raise BlobError('noncanonical head')
        if major==0:return n
        if major==1:return -1-n
        if major in (2,3):
            if pos+n>len(data):raise BlobError('truncated bytes')
            b=data[pos:pos+n];pos+=n
            if major==2:return b
            try:return b.decode('utf-8')
            except UnicodeDecodeError as e:raise BlobError('UTF-8') from e
        if major==4:
            if n>1024:raise BlobError('array limit')
            return [read(depth+1) for _ in range(n)]
        if major==6 and allow_tag:return Tag(n,read(depth+1))
        raise BlobError('unsupported tag/map')
    result=read()
    if pos!=len(data) or encode(result)!=data:raise BlobError('noncanonical/trailing bytes')
    return result

def content_id(data: bytes) -> tuple[int,str]:
    return len(data),hashlib.sha256(data).hexdigest()

def object_id(kind: str, data: bytes, algorithm='sha1') -> str:
    return hashlib.new(algorithm,(kind+' '+str(len(data))).encode()+b'\0'+data).hexdigest()

def tree_bytes(entries: dict[str,tuple[str,str]],algorithm='sha1') -> bytes:
    out=[]
    for name,(kind,oid) in sorted(entries.items(),key=lambda kv:kv[0].encode()+(b'/' if kv[1][0]=='tree' else b'\0')):
        if not name or '/' in name or '\0' in name:raise BlobError('tree name')
        if kind not in ('blob','tree'):raise BlobError('tree kind')
        raw=bytes.fromhex(oid)
        if len(raw)!=(20 if algorithm=='sha1' else 32):raise BlobError('OID width')
        out.append((b'40000' if kind=='tree' else b'100644')+b' '+name.encode()+b'\0'+raw)
    return b''.join(out)

def parse_tree(data: bytes,algorithm='sha1') -> dict[str,tuple[str,str]]:
    out={};pos=0;width=20 if algorithm=='sha1' else 32
    while pos<len(data):
        try:
            sep=data.index(b' ',pos);end=data.index(b'\0',sep)
            mode=data[pos:sep];name=data[sep+1:end].decode('ascii')
        except (ValueError,UnicodeDecodeError) as e:raise BlobError('malformed tree') from e
        if mode not in (b'40000',b'100644') or name in out or '/' in name:raise BlobError('mode/name')
        pos=end+1
        if pos+width>len(data):raise BlobError('truncated OID')
        oid=data[pos:pos+width].hex();pos+=width
        out[name]=('tree' if mode==b'40000' else 'blob',oid)
    if tree_bytes(out,algorithm)!=data:raise BlobError('tree order')
    return out

class MemoryStore:
    def __init__(self,algorithm='sha1'):
        self.algorithm=algorithm;self.objects={};self.reads=[]
    def put(self,kind,data):
        oid=object_id(kind,data,self.algorithm);self.objects[oid]=(kind,data);return oid
    def get(self,oid):
        self.reads.append(oid)
        if oid not in self.objects:raise BlobError('unavailable object')
        kind,data=self.objects[oid]
        if object_id(kind,data,self.algorithm)!=oid:raise BlobError('corrupt Git object')
        return kind,data
    def tree(self,entries):return self.put('tree',tree_bytes(entries,self.algorithm))

def _get(store,oid,kind):
    actual,data=store.get(oid)
    if actual!=kind:raise BlobError('wrong object type')
    return data

def _tree(store,oid):return parse_tree(_get(store,oid,'tree'),store.algorithm)

GEAR=[int.from_bytes(hashlib.sha256(b'orna.gear.v1\0'+bytes([i])).digest()[:8],'big') for i in range(256)]
def gear_chunks(parts: Iterable[bytes]) -> Iterable[bytes]:
    h=0;buf=bytearray()
    for part in parts:
        for b in part:
            buf.append(b);h=((h<<1)+GEAR[b])&((1<<64)-1)
            if len(buf)>=65536 and ((h&262143)==0 or len(buf)==MAX_CHUNK):
                yield bytes(buf);buf.clear();h=0
    if buf:yield bytes(buf)

def fixed_chunks(data: bytes,size: int) -> Iterable[bytes]:
    if not 1<=size<=MAX_CHUNK:raise BlobError('chunk size')
    for pos in range(0,len(data),size):yield data[pos:pos+size]

@dataclass
class Built:
    root: str
    digest: str
    length: int
    chunks: list[tuple[str,int]]

def build(store,parts: Iterable[bytes]) -> Built:
    entries=[];h=hashlib.sha256();length=0;chunks=[]
    for part in parts:
        if not part or len(part)>MAX_CHUNK:raise BlobError('chunk length')
        length+=len(part)
        if length>MAX_LENGTH:raise BlobError('content length')
        h.update(part);oid=store.put('blob',part);chunks.append((oid,len(part)))
        entries.append((len(part),oid,hashlib.sha256(part).digest()))
    def node(group,height):
        end=0;index=[];tree={}
        for i,(n,oid,digest) in enumerate(group):
            end+=n;index.append([end,bytes.fromhex(oid),digest]);tree[f'{i:03x}']=('blob' if height==0 else 'tree',oid)
        tree['index']=('blob',store.put('blob',encode([1,height,end,index])))
        return end,store.tree(tree),None
    nodes=[node(entries[i:i+FANOUT],0) for i in range(0,len(entries),FANOUT)] or [node([],0)]
    height=0
    while len(nodes)>1:
        height+=1
        if height>8:raise BlobError('height')
        nodes=[node(nodes[i:i+FANOUT],height) for i in range(0,len(nodes),FANOUT)]
    _,data_oid,_=nodes[0]
    meta=store.put('blob',encode([1,h.digest(),length,bytes.fromhex(data_oid)]))
    root=store.tree({'meta':('blob',meta),'data':('tree',data_oid)})
    return Built(root,h.hexdigest(),length,chunks)

def metadata(store,root):
    t=_tree(store,root)
    if set(t)!={'meta','data'} or t['meta'][0]!='blob' or t['data'][0]!='tree':raise BlobError('root shape')
    m=decode(_get(store,t['meta'][1],'blob'))
    if not isinstance(m,list) or len(m)!=4 or m[0]!=1 or type(m[0]) is not int:raise BlobError('meta shape')
    if not isinstance(m[1],bytes) or len(m[1])!=32 or type(m[2]) is not int or not 0<=m[2]<=MAX_LENGTH:raise BlobError('meta identity')
    if m[3]!=bytes.fromhex(t['data'][1]):raise BlobError('meta/tree mismatch')
    return m[1].hex(),m[2],t['data'][1]

def node(store,oid,expected_length=None,expected_height=None):
    t=_tree(store,oid)
    if 'index' not in t or t['index'][0]!='blob':raise BlobError('node index')
    m=decode(_get(store,t['index'][1],'blob'))
    if not isinstance(m,list) or len(m)!=4 or m[0]!=1 or type(m[0]) is not int:raise BlobError('node shape')
    _,height,total,entries=m
    if type(height) is not int or not 0<=height<=8 or type(total) is not int or not 0<=total<=MAX_LENGTH:raise BlobError('node range')
    if expected_length is not None and expected_length!=total:raise BlobError('child length')
    if expected_height is not None and expected_height!=height:raise BlobError('child height')
    if not isinstance(entries,list) or len(entries)>FANOUT or (not entries and (height!=0 or total!=0)):raise BlobError('fanout')
    if set(t)!={'index',*(f'{i:03x}' for i in range(len(entries)))}:raise BlobError('child names')
    end=0;result=[]
    for i,e in enumerate(entries):
        if not isinstance(e,list) or len(e)!=3:raise BlobError('entry shape')
        stop,target,digest=e;kind='blob' if height==0 else 'tree'
        if type(stop) is not int or not end<stop<=MAX_LENGTH:raise BlobError('entry interval')
        if not isinstance(target,bytes) or target!=bytes.fromhex(t[f'{i:03x}'][1]) or t[f'{i:03x}'][0]!=kind:raise BlobError('entry target')
        if height==0:
            if not isinstance(digest,bytes) or len(digest)!=32 or stop-end>MAX_CHUNK:raise BlobError('chunk descriptor')
        elif digest is not None:raise BlobError('internal digest')
        result.append((end,stop,target.hex(),digest));end=stop
    if end!=total:raise BlobError('node total')
    return height,total,result

def _walk(store,oid,start,stop,total=None,height=None):
    h,n,entries=node(store,oid,total,height)
    for a,b,child,digest in entries:
        if b<=start or a>=stop:continue
        if h==0:
            data=_get(store,child,'blob')
            if len(data)!=b-a or hashlib.sha256(data).digest()!=digest:raise BlobError('chunk integrity')
            yield data[max(start-a,0):min(stop-a,b-a)]
        else:yield from _walk(store,child,max(0,start-a),min(stop-a,b-a),b-a,h-1)

def read_range(store,root,offset,length):
    digest,total,oid=metadata(store,root)
    if type(offset) is not int or type(length) is not int or offset<0 or offset>total or not 0<=length<=MAX_READ:raise BlobError('range bound')
    if length==0 or offset==total:return b''
    result=b''.join(_walk(store,oid,offset,min(total,offset+length),total))
    if len(result)!=min(length,total-offset):raise BlobError('range completeness')
    return result

def verify(store,root):
    digest,total,oid=metadata(store,root)
    h=hashlib.sha256();n=0
    # Even an empty root's index must be validated by full verification.
    node(store,oid,total)
    for part in _walk(store,oid,0,total,total):h.update(part);n+=len(part)
    if n!=total or h.hexdigest()!=digest:raise BlobError('whole-content mismatch')
    return digest

def stored_reference(digest: str,length: int):
    if len(digest)!=64 or any(x not in '0123456789abcdef' for x in digest) or not 0<=length<=MAX_LENGTH:raise BlobError('reference')
    return Tag(60100,[bytes.fromhex(digest),length])

def portable_blob_decode(data):
    value=decode(data,allow_tag=False,maximum=MAX_READ+16)
    if not isinstance(value,bytes):raise BlobError('not portable Blob')
    return value

def deterministic_bytes(n:int) -> bytes:
    """Small reference fixtures only: SHA-256 counter expansion, seed fixed."""
    return b''.join(hashlib.sha256(b'orna.blob.fixture.v1\0'+i.to_bytes(8,'big')).digest() for i in range((n+31)//32))[:n]
