#!/usr/bin/env python3
"""Generate the human-readable sys reference from the shipped API schema."""
from pathlib import Path
import json,re
ROOT=Path(__file__).resolve().parents[1]
def slug(s):return re.sub(r'[^a-z0-9]+','-',s.lower()).strip('-')
def cell(s):return str(s).replace('|','\\|').replace('\n',' ')
def main():
 p=ROOT/'api/sys.json';j=json.loads(p.read_text())
 j['source_of_truth']='api/sys.json; normative semantics in source chapters and generated system reference'
 for k in ['singletons','opaque_identifiers','reference_aliases','value_types','enums','relations','functions','failure_codes']:j['counts'][k]=len(j[k])
 p.write_text(json.dumps(j,indent=2)+'\n')
 out=['# System API reference {#system-reference}',
 '''This reference defines system names, field types and callable interfaces. The [system model](#system) explains identity, snapshots, reflection and redaction; [administration](#administration) specifies state-changing preconditions. A machine-readable schema is available in [`api/sys.json`](api/sys.json).

Signatures use qualified callable names and type parameters such as `T`. Source declaration syntax is defined by the [grammar](#grammar). Optional types use `?`; their absence value is `null`. All relation rows are read-only.

Field types and declared invariants apply together. Source, host-path and payload fields also follow availability and redaction rules. `null` represents defined absence; unavailable mandatory data produces the specified failure.''',
 '## Namespace index',
 '| Group | Use |','|---|---|',
 '| `sys.database`, `sys.current`, `sys.rt`, `sys.repl` | Current attachment, activation, runtime and optional REPL observations. |',
 '| `sys.catalog` | Declarations, types, source and dependencies. |',
 '| `sys.history`, `sys.git` | Snapshots, semantic changes and Git objects. |',
 '| `sys.storage`, `sys.build` | Single-store physical observations and build/test metadata. |',
 '| Root functions | Typed resolution, inspection, invocation and execution control. |',
 '| `sys.admin` | Explicit state transitions; never row setters. |',
 '## Singleton views', '| Name | Type | Availability |','|---|---|---|']
 for x in j['singletons']:out.append(f'| `{x["name"]}` | `{x["type"]}` | {cell(x.get("availability",""))} |')
 out+=['## Opaque identifiers','An opaque identifier cannot be implicitly substituted for a string, UUID or another identifier. Its domain and canonical encoding remain part of the named type.']
 for x in j['opaque_identifiers']:
  if isinstance(x,str):out.append('### `'+x+'` {#api-type-'+slug(x)+'}\n\nNominal identity value; see [identity and encoding](#formats).')
  else:out.append('### `'+x['name']+'` {#api-type-'+slug(x['name'])+'}\n\n'+x.get('purpose',x.get('description','Opaque nominal identifier.')))
 out+=['## Typed row-reference aliases','Each alias is a pinned `sys.RowRef<T>` for the named canonical relation. It is not an implicit conversion from the row value. Obtain it with `.reference`.','| Alias | Target |','|---|---|']
 for x in j['reference_aliases']:
  if isinstance(x,str):out.append(f'| `{x}` | See its relation. |')
  else:out.append(f'| `{x["name"]}` | `{x.get("target",x.get("type",x.get("row_type","")))}` |')
 for group,title in [('enums','Closed enumerations'),('value_types','Supporting value types'),('relations','Canonical system relations')]:
  out.append('## '+title)
  entries = ([{'name': k, 'variants': v} for k,v in j[group].items()] if isinstance(j[group],dict) else j[group])
  for x in entries:
   n=x['name'];out.append('### `'+n+'` {#api-type-'+slug(n)+'}')
   out.append(x.get('purpose',''))
   if group=='enums':
    vs=x.get('variants',x.get('values',[]));out.append('Values: '+', '.join('`'+(v if isinstance(v,str) else v['name'])+'`' for v in vs)+'.')
    if x.get('description'):out.append(x['description'])
   else:
    if group=='relations':
     out.append(f'**Grouped handle:** `{x["grouped_handle"]}`. **Availability:** {x["availability"]}. **Natural key:** `{x["key"]}`. **Reference:** `{x["reference_type"]}`.')
     if x['grouped_handle'].startswith('sys.rt.'):
      out.append('The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.')
    fs=x.get('fields',[])
    if fs:
     out+=['| Field | Type |','|---|---|']
     out += [f'| `{f["name"]}` | `{cell(f["type"])}` |' for f in fs]
    elif group=='value_types':out.append('This type has no public structural fields; use the operations that explicitly accept it.')
   for inv in x.get('invariants',[]):out.append('Invariant: '+inv.rstrip('.')+'.')
 out+=['## Callable operations', 'Read operations do not mutate database state. Invocation/start operations follow [INVOKE-1](#system); admin operations follow [administrative transitions](#administration). Resolution and retained-data failures are ordinary typed failures, not missing/empty successful results.']
 for x in j['functions']:
  n=x['name'];out+=['### `'+n+'` {#api-call-'+slug(n)+'}',x.get('purpose',''),'```text\n'+x['signature']+'\n```','**Effect:** '+x['effect']+'.']
  for key in ['preconditions','snapshot','ownership','contract','invariants']:
   v=x.get(key)
   if v:
    if isinstance(v,list):v='; '.join(v)
    if key=='contract':v='[Blob value and storage contract](#blob-values).' if n.startswith('sys.blob.') else '[Administrative transition contract](#administration).'
    out.append(key.capitalize()+': '+v)
  if x['effect']=='admin' or n.startswith('sys.admin.'):
   out.append('State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).')
  elif n.startswith(('sys.invoke','sys.start','sys.await','sys.cancel')):
   out.append('Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).')
 out+=['## Portable failure codes','A failure code is stable machine-readable classification. The message may be localised; the code must not be replaced by vendor-only wording. Unless a more specific transition is stated, failure before admission performs no requested mutation.']
 meanings={
 'sys.version.incompatible':'Required API/profile coordinate is unavailable.', 'sys.context.repl_unavailable':'The operation requires a REPL context that is absent.',
 'sys.object.not_found':'No object matches the exact accessible selector.', 'sys.object.ambiguous':'The selector matches more than one allowed object.', 'sys.object.wrong_kind':'The resolved object is not the requested kind.',
 'sys.snapshot.not_found':'The snapshot selector cannot be resolved.', 'sys.snapshot.incomplete':'Required snapshot objects are absent or unavailable.', 'sys.snapshot.expired':'A CWD pin is no longer retained and cannot be rebound.',
 'sys.source.unavailable':'Required source bytes or maps are not retained/available.', 'sys.source.redacted':'The requested source content is protected.',
 'sys.handle.foreign_runtime':'A live handle belongs to another runtime generation.', 'sys.handle.expired':'A same-runtime handle no longer names a live retained operation.',
 'sys.invoke.argument_missing':'A required argument has no supplied value or default.', 'sys.invoke.argument_unknown':'An argument name is not declared by the target.', 'sys.invoke.argument_type':'An argument has an incompatible exact type or invalid duplicate binding.',
 'sys.invoke.return_type':'The result witness disagrees with the declared result before effects.', 'sys.invoke.effect_unavailable':'The selected context does not permit a target effect.', 'sys.invoke.idempotency_mismatch':'The key is already bound to a different complete invocation identity.',
 'sys.invoke.await_timeout':'Waiting expired; the target was not cancelled by the wait.', 'sys.invoke.not_callable':'The resolved object is not an invocable function.', 'sys.invoke.revision_unavailable':'The pinned target revision cannot be loaded.',
 'sys.invoke.snapshot_mismatch':'An explicit snapshot conflicts with the function reference or inherited transaction pin.', 'sys.invoke.transaction_mode':'The requested transaction mode is invalid, including asynchronous inheritance.',
 'sys.checkpoint.not_found':'A required checkpoint reference does not resolve. The optional lookup function instead returns null for known absence.', 'sys.checkpoint.conflict':'The checkpoint version/position or active lease differs from its expected state.', 'sys.checkpoint.not_replayable':'The provider cannot resume/reset to the requested position.',
 'sys.failure.state_conflict':'The requested failure transition is not allowed from this state.', 'sys.failure.skip_unsupported':'The provider cannot safely advance past this delivery.', 'sys.failure.stale_version':'The failure changed after the caller observed its version.',
 'sys.admin.read_only':'The attachment cannot perform the requested administrative mutation.', 'sys.admin.busy':'An incompatible owner/lease or reentrant write activation prevents admission.', 'sys.admin.assertion_failed':'The candidate state violates a required assertion.', 'sys.admin.missing_object':'Required objects cannot be obtained before admission.', 'sys.admin.verification_failed':'Verification could not complete; no clean result is asserted.',
 'sys.storage.corrupt':'Physical bytes, schema, manifest or digest contradict the profile.', 'sys.storage.unavailable':'Required physical data cannot be obtained.', 'sys.storage.unrepresentable_key':'The selected input/format operation cannot encode the key within its declared bounds; a VFS filename limit alone must not reject a valid stored key.', 'sys.storage.rewrite_conflict':'Inputs changed, collide or became stale before rewrite admission.',
 'sys.redaction.denied':'A generic operation attempted to reveal protected content.', 'sys.git.unborn_head':'No default committed HEAD exists.', 'sys.git.uncommitted_snapshot':'A branch/checkout target does not identify a commit.', 'sys.git.dirty_conflict':'Local staged, unstaged or pending changes would be overwritten.', 'sys.git.stale_plan':'The checkout preview no longer matches current state.', 'sys.git.invalid_ref':'The proposed local branch name is invalid under the adopted Git ref rules.', 'sys.git.branch_exists':'Branch creation expected nonexistence but the branch is already present.', 'sys.git.nothing_to_commit':'No staged change is present for this nonempty-commit operation.'}
 out+=['| Code | Condition |','|---|---|']
 for c in j['failure_codes']:
  if c not in meanings: meanings[c] = json.loads((ROOT/'api/additions-1.1.json').read_text())['failure_conditions'][c]
  out.append(f'| `{c}` | {meanings[c]} |')
 p=ROOT/'source/34-system-reference.md';p.write_text('\n\n'.join(out)+'\n')
 # Consecutive table lines need single newlines, not blank paragraph separators.
 s=p.read_text();s=re.sub(r'(?m)^(\|[^\n]+\|)\n\n(?=\|)',r'\1\n',s)
 # Repeat until alternating table gaps are gone.
 while re.search(r'(?m)^\|[^\n]+\|\n\n\|',s):s=re.sub(r'(?m)^(\|[^\n]+\|)\n\n(?=\|)',r'\1\n',s)
 p.write_text(s)
 m=json.loads((ROOT/'source/manifest.json').read_text());m=[x for x in m if x['id']!='system-reference']
 at=next(i for i,x in enumerate(m) if x['id']=='grammar')
 m.insert(at,{'id':'system-reference','title':'System API reference','part':'Reference','file':'source/34-system-reference.md'})
 for i,x in enumerate(m,1):
  f=ROOT/x['file'];t=f.read_text();t=re.sub(r'^# [^\n]+',f'# {i}. {x["title"]} {{#{x["id"]}}}',t,count=1);f.write_text(t)
 (ROOT/'source/manifest.json').write_text(json.dumps(m,indent=2)+'\n')
 print('Generated reference:',len(s),'bytes;',len(j['relations']),'relations;',len(j['functions']),'signatures')
if __name__=='__main__':main()
