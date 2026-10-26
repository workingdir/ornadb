#!/usr/bin/env python3
"""Bounded, independently executable OVB-1/CBOR reference model.

This is not an Orna codec implementation: it tests byte structure, a selected tag
subset, canonical decimals/options and snapshot pins. See evidence for scope.
"""
from dataclasses import dataclass
import math,struct,hashlib
from typing import Any
@dataclass(frozen=True)
class Tag:
    number:int
    value:Any

def head(major:int,n:int)->bytes:
    if n<0:raise ValueError('negative argument')
    if n<24:return bytes([(major<<5)|n])
    for ai,width in [(24,1),(25,2),(26,4),(27,8)]:
        if n < 1<<(8*width):return bytes([(major<<5)|ai])+n.to_bytes(width,'big')
    raise ValueError('length/tag outside uint64')

def encode(value:Any)->bytes:
    if value is None:return b'\xf6'
    if value is False:return b'\xf4'
    if value is True:return b'\xf5'
    if isinstance(value,int):
        arg=value if value>=0 else -1-value
        if arg < 1<<64:return head(0 if value>=0 else 1,arg)
        data=arg.to_bytes((arg.bit_length()+7)//8,'big')
        return head(6,2 if value>=0 else 3)+encode(data)
    if isinstance(value,float):
        data=bytes.fromhex('7ff8000000000000') if math.isnan(value) else struct.pack('>d',value)
        return b'\xfb'+data
    if isinstance(value,bytes):return head(2,len(value))+value
    if isinstance(value,str):
        data=value.encode('utf-8','strict');return head(3,len(data))+data
    if isinstance(value,(list,tuple)):return head(4,len(value))+b''.join(encode(x) for x in value)
    if isinstance(value,dict):
        entries=sorted((encode(k),encode(v)) for k,v in value.items())
        if len({k for k,_ in entries})!=len(entries):raise ValueError('duplicate encoded key')
        return head(5,len(entries))+b''.join(k+v for k,v in entries)
    if isinstance(value,Tag):return head(6,value.number)+encode(value.value)
    raise TypeError(type(value).__name__)

class Reader:
    def __init__(self,data:bytes,max_depth=64,max_items=100000,max_bytes=16777216):
        if len(data)>max_bytes:raise ValueError('bytes bound')
        self.data=data;self.pos=0;self.depth=max_depth;self.items=max_items
    def take(self,n):
        if n<0 or self.pos+n>len(self.data):raise ValueError('truncated')
        out=self.data[self.pos:self.pos+n];self.pos+=n;return out
    def item(self,depth=0):
        if depth>self.depth:raise ValueError('depth bound')
        self.items-=1
        if self.items<0:raise ValueError('item bound')
        b=self.take(1)[0];major=b>>5;ai=b&31
        if major==7:
            if ai==20:return False
            if ai==21:return True
            if ai==22:return None
            if ai==27:
                bits=self.take(8);v=struct.unpack('>d',bits)[0]
                if math.isnan(v) and bits.hex()!='7ff8000000000000':raise ValueError('noncanonical NaN')
                return v
            raise ValueError('unsupported/noncanonical simple or short float')
        if ai<24:n=ai
        elif ai in (24,25,26,27):
            width={24:1,25:2,26:4,27:8}[ai];n=int.from_bytes(self.take(width),'big')
            if n < {24:24,25:256,26:65536,27:4294967296}[ai]:raise ValueError('nonminimal argument')
        else:raise ValueError('indefinite/reserved')
        if major==0:return n
        if major==1:return -1-n
        if major==2:return self.take(n)
        if major==3:return self.take(n).decode('utf-8','strict')
        if major==4:
            if n>self.items:raise ValueError('collection bound')
            return [self.item(depth+1) for _ in range(n)]
        if major==5:
            if n*2>self.items:raise ValueError('map bound')
            result={};previous=None
            for _ in range(n):
                start=self.pos;k=self.item(depth+1);raw=self.data[start:self.pos]
                if previous is not None and raw<=previous:raise ValueError('unordered or duplicate key')
                previous=raw
                if not isinstance(k,(str,bytes,int)) or isinstance(k,bool):raise ValueError('model map key subset')
                result[k]=self.item(depth+1)
            return result
        if major==6:
            v=self.item(depth+1)
            if n in (2,3):
                if not isinstance(v,bytes) or len(v)<9 or v[0]==0:raise ValueError('nonminimal bignum')
                x=int.from_bytes(v,'big');return x if n==2 else -1-x
            tag=Tag(n,v);validate_tag(tag);return tag
        raise ValueError('major type')

def decode(data:bytes):
    r=Reader(data);value=r.item()
    if r.pos!=len(data):raise ValueError('trailing bytes')
    if encode(value)!=data:raise ValueError('not canonical')
    return value

def decimal(coefficient:int,exponent:int)->Tag:
    if not isinstance(coefficient,int) or not isinstance(exponent,int):raise TypeError('decimal parts')
    if coefficient==0:exponent=0
    else:
        while coefficient%10==0:coefficient//=10;exponent+=1
    return Tag(60000,[coefficient,exponent])

def validate_tag(t:Tag):
    n,v=t.number,t.value
    if n==37:
        if not isinstance(v,bytes) or len(v)!=16:raise ValueError('uuid width')
    elif n==60000:
        if not isinstance(v,list) or len(v)!=2 or any(type(x)!=int for x in v):raise ValueError('decimal structure')
        if decimal(*v)!=t:raise ValueError('non-normal decimal')
    elif n==60013:
        if not isinstance(v,list) or not v or type(v[0])!=int or not ((v[0]==0 and len(v)==1) or (v[0]==1 and len(v)==2)):raise ValueError('option structure')
    elif n==60014:
        if v!=[]:raise ValueError('unit structure')
    elif n==60015:
        if not isinstance(v,list) or not v:raise ValueError('tuple structure')
    elif n not in range(60000,60028):raise ValueError("unregistered tag")
    # Other registered tags are structurally retained, not fully type-validated.

def validate_snapshot(s):
    if not isinstance(s,list) or not s or type(s[0])!=int:raise ValueError('snapshot structure')
    if s[0]==0:
        if len(s)!=5 or not all(isinstance(s[i],Tag) and s[i].number==37 for i in [1,2]) or type(s[3])!=int or s[3]<0 or not isinstance(s[4],bytes) or len(s[4])!=32:raise ValueError('CWD pin')
        if digest('orna.snapshot.v1',s[:4])!=s[4]:raise ValueError('CWD pin digest')
    elif s[0]==1:
        if len(s)!=4 or not isinstance(s[1],Tag) or s[1].number!=37 or s[2] not in ('sha1','sha256') or not isinstance(s[3],bytes) or len(s[3])!={'sha1':20,'sha256':32}[s[2]]:raise ValueError('commit pin')
    else:raise ValueError('unknown snapshot')
    return s

def digest(domain:str,v:Any)->bytes:
    return hashlib.sha256(domain.encode('ascii')+b'\0'+encode(v)).digest()
