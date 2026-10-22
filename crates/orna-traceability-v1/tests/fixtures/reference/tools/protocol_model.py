#!/usr/bin/env python3
"""Selected structural/state models; not a network server or full typed decoder."""
from pathlib import Path
import copy,hashlib,json
from jsonschema import Draft202012Validator
from value_model import Tag,encode,decode,validate_snapshot,digest
R=Path(__file__).resolve().parents[1];J=json.loads((R/'profiles/live-messages.json').read_text());REG={r['code']:r for r in J['messages']};results=[]
def require(c,msg='invalid'): 
 if not c:raise ValueError(msg)
def byte(v,n):return isinstance(v,bytes) and len(v)==n
def integer(v):return type(v)==int

def typed(t,v):
 if t.endswith('?'):
  if v is None:return
  return typed(t[:-1],v)
 if t=='UInt':require(integer(v) and v>=0)
 elif t=='PositiveUInt':require(integer(v) and v>0)
 elif t=='Str':require(isinstance(v,str))
 elif t=='Bool':require(type(v)==bool)
 elif t in ['bytes16','bytes32']:require(byte(v,int(t[5:])))
 elif t=='Uuid':require(isinstance(v,Tag) and v.number==37 and byte(v.value,16))
 elif t in ['TargetKind','ResultStatus','RequestState']:require(integer(v) and v in J['definitions'][t])
 elif t=='Snapshot':validate_snapshot(v)
 elif t=='DatabaseContext':
  require(type(v)==dict and set(v)=={0,1});typed('Uuid',v[0]);typed('Snapshot?',v[1])
 elif t=='PresentationContext':
  require(type(v)==dict and set(v)==set(range(5)))
  for key,typ in [(0,'Str'),(1,'Str?'),(2,'PositiveUInt?'),(3,'Str'),(4,'[Str]')]:typed(typ,v[key])
 elif t=='[Str]':require(isinstance(v,list));[typed('Str',x) for x in v]
 elif t=='NonnegativeDuration':require(isinstance(v,Tag) and v.number==60005 and isinstance(v.value,list) and len(v.value)==2 and integer(v.value[0]) and v.value[0]>=0 and integer(v.value[1]) and 0<=v.value[1]<1000000000)
 elif t in ['TypedValue','TypedValueOrNull']:
  require(v is not None or t.endswith('OrNull'));encode(v)
 elif t=='DiagnosticOrNull':
  if v is not None:typed('Diagnostic',v)
 elif t=='Diagnostic':
  require(isinstance(v,Tag) and v.number==60011);b=v.value;require(type(b)==dict and set(range(7))<=set(b) and set(b)<={0,1,2,3,4,5,6,7})
  typed('Str',b[0]);require(integer(b[1]) and 0<=b[1]<=4);typed('Str',b[2]);require(isinstance(b[3],list));typed('[Str]',b[4]);require(isinstance(b[5],list));typed('Bool',b[6]);[typed('Diagnostic',c) for c in b[5]]
  for span in b[3]:require(isinstance(span,list) and len(span)==4);typed('Snapshot',span[0]);typed('Str',span[1]);require(integer(span[2]) and integer(span[3]) and 0<=span[2]<=span[3])
  if 7 in b:typed('Uuid',b[7])
 elif t=='PresentNode':
  require(isinstance(v,Tag) and v.number==60012 and isinstance(v.value,list) and len(v.value)==4);kind,key,props,children=v.value
  require(isinstance(kind,str) or isinstance(kind,Tag) and kind.number==37);require(isinstance(props,dict) and isinstance(children,list));[typed('PresentNode',c) for c in children]
  keys=[encode(c.value[1]) for c in children if c.value[1] is not None];require(len(keys)==len(set(keys)),'duplicate child key')
 elif t=='PatchList':
  require(isinstance(v,list))
  for op in v:
   require(isinstance(op,list) and len(op)>=2 and integer(op[0]) and op[0] in range(4));require(len(op)==(2 if op[0]==1 else 3));require(isinstance(op[1],list))
 elif t=='ResultBody':body(REG[18],v)
 else:raise ValueError('unsupported model type '+t)
def body(spec,b):
 require(type(b)==dict);require(all(integer(k) and 0<=k<=65535 for k in b));req={int(k):v for k,v in spec['required'].items()};opt={int(k):v for k,v in spec['optional'].items()};require(set(req)<=set(b))
 require(not any(k>=32768 and k not in req and k not in opt for k in b))
 for k,t in (req|opt).items():
  if k in b:typed(t,b[k])
 if spec['code']==17:require(b[1]>b[0])
 if spec['code']==18:
  if b[0]==0:require(b[1] is not None and b[3] is None)
  else:require(b[1] is None)

def validate(msg,direction):
 require(type(msg)==dict and set(msg)=={0,1,2,3,4});require(msg[0]==1 and integer(msg[0]));require(integer(msg[1]) and msg[1] in REG);spec=REG[msg[1]];require(spec['direction']==direction)
 for key,policy in [(2,spec['request']),(3,spec['watch'])]:
  if policy=='null':require(msg[key] is None)
  elif policy=='required':require(byte(msg[key],16))
  else:require(msg[key] is None or byte(msg[key],16))
 body(spec,msg[4])
 if msg[1]==6:require(msg[2]!=msg[4][1])
 return True

def check(name,fn):
 try:detail=fn();results.append({'name':name,'pass':True,'detail':detail})
 except Exception as e:results.append({'name':name,'pass':False,'error':str(e)})
def rejects(fn):
 try:fn()
 except (ValueError,TypeError,KeyError):return
 raise AssertionError('must reject')
ID=b'A'*16;WATCH=b'W'*16;FP=b'F'*32;DB=Tag(37,b'D'*16);SN=[0,DB,Tag(37,b'R'*16),1];SN.append(digest('orna.snapshot.v1',SN));PC={0:'en',1:'UTC',2:80,3:'terminal/default',4:['text']};NODE=Tag(60012,['text',None,{'text':'hello'},[]]);DIAG=Tag(60011,{0:'wire.read_only',1:3,2:'not permitted',3:[],4:[],5:[],6:False})
BODIES={0:{0:ID,1:PC},1:{},2:{},3:{0:0,1:ID,2:Tag(60014,[]),3:FP},4:{0:'1 + 2',1:{0:DB,1:SN},2:PC,3:FP},5:{0:'1 + 2',1:{0:DB,1:None},2:PC,3:Tag(60005,[1,0])},6:{0:0,1:b'B'*16},7:{0:ID,1:FP},16:{0:0,1:NODE,2:SN},17:{0:0,1:1,2:[[2,[],NODE]],3:SN},18:{0:0,1:Tag(60014,[]),2:FP,3:None},19:{0:DIAG},20:{0:ID,1:2,2:FP,3:None}}
def vectors():
 out=[]
 for code,spec in REG.items():
  msg={0:1,1:code,2:None if spec['request']=='null' else ID,3:WATCH if spec['watch']=='required' else None,4:BODIES[code]}
  validate(msg,spec['direction']);encoded=encode(msg);validate(decode(encoded),spec['direction']);out.append({'name':spec['name'],'direction':spec['direction'],'hex':encoded.hex()})
 (R/'tests/protocol-vectors.json').write_text(json.dumps(out,indent=2)+'\n')
 return {'message_kinds':len(out),'scope':'envelope, known fields, selected nested values and canonical byte roundtrip'}
check('wire.all-message-structures',vectors)
def malformed():
 base={0:1,1:4,2:ID,3:None,4:BODIES[4]};n=0
 def fail(m,d='client'):
  nonlocal n;rejects(lambda:validate(m,d));n+=1
 for key,value in [(0,2),(1,999),(2,'not-bytes'),(3,WATCH),(4,{0:'x'})]:m=copy.deepcopy(base);m[key]=value;fail(m)
 m=copy.deepcopy(base);m[5]=0;fail(m);fail(base,'host')
 m=copy.deepcopy(base);m[4][32768]=True;fail(m)
 m=copy.deepcopy(base);m[4][3]=b'bad';fail(m)
 m=copy.deepcopy(base);m[4][1][1]=[0];fail(m)
 m=copy.deepcopy(base);m[4][2][2]=-1;fail(m)
 m=copy.deepcopy(base);m[4][42]='optional';validate(m,'client')
 m={0:1,1:6,2:ID,3:None,4:{0:0,1:ID}};fail(m)
 m={0:1,1:17,2:None,3:WATCH,4:{0:4,1:4,2:[],3:SN}};fail(m,'host')
 return {'invalid_messages_rejected':n,'unknown_optional_extension_accepted':True}
check('wire.invalid-structure-rejections',malformed)
def session_json():
 s=json.loads((R/'profiles/session.schema.json').read_text());v=Draft202012Validator(s);req={'database':'12345678-1234-1234-1234-123456789abc','protocol':'orna.present.v1'};v.validate(req)
 bad=copy.deepcopy(req);bad['path']='/etc';assert list(v.iter_errors(bad));bad={'database':req['database'],'protocol':'other'};assert list(v.iter_errors(bad))
 return 'Closed create/resume/response schemas; unknown fields and unsupported protocol reject. Authentication/cookies are implementation obligations.'
check('session.http-schema',session_json)
def identity():
 records={};writes={};key=(b'SESSION',ID);fp=hashlib.sha256(encode(['input',1])).digest()
 def reserve(f):
  if key in records and records[key]['fingerprint']!=f:raise ValueError('wire.request_mismatch')
  return records.setdefault(key,{'fingerprint':f,'state':'reserved'})
 r=reserve(fp);assert reserve(fp) is r;rejects(lambda:reserve(b'X'*32))
 # Crash with reservation only has no automatic re-execution permission.
 r['state']='orphaned';assert writes=={};r['state']='running'
 wc=copy.deepcopy(writes);rc=copy.deepcopy(records);wc['answer']=3;rc[key].update(state='terminal',result=3);writes,records=wc,rc
 assert reserve(fp)['result']==3 and writes=={'answer':3}
 return 'Fingerprint mismatch rejects; a reservation is not success or a replay licence; terminal claim and model writes become visible together.'
check('request.reservation-and-terminal-claim',identity)
def patch_atomic():
 current={'revision':4,'node':{'text':'old','count':0}}
 def apply(base,new,ops):
  if base!=current['revision']:raise ValueError('resync')
  candidate=copy.deepcopy(current)
  for k,val in ops:
   if k not in candidate['node']:raise ValueError('resync')
   candidate['node'][k]=val
  candidate['revision']=new;current.update(candidate)
 before=copy.deepcopy(current);rejects(lambda:apply(4,5,[('text','new'),('missing',0)]));assert current==before;rejects(lambda:apply(3,5,[('text','new')]));assert current==before
 apply(4,5,[('text','new')]);assert current['node']['text']=='new'
 return 'Selected property-patch model publishes atomically and preserves the visible tree on invalid path or revision mismatch.'
check('watch.atomic-delta-resync',patch_atomic)
report={'scope':'Selected protocol structural and state models, not independent clients/servers, complete tagged-value validation, authentication or malformed-WebSocket fuzzing.','passed':sum(r['pass'] for r in results),'total':len(results),'results':results}
(R/'evidence/protocol-models.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2));raise SystemExit(0 if report['passed']==report['total'] else 1)
