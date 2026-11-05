#!/usr/bin/env python3
"""Real-Git regression tests of ref/index boundaries, not an Orna publisher."""
from pathlib import Path
import subprocess,tempfile,os,shutil
FIXED={'GIT_AUTHOR_DATE':'2026-09-05T00:00:00Z','GIT_COMMITTER_DATE':'2026-09-05T00:00:00Z'}
def tests():
 results=[]
 def check(name,fn):
  try:detail=fn();results.append({'name':name,'pass':True,'detail':detail})
  except Exception as ex:results.append({'name':name,'pass':False,'error':str(ex)})
 with tempfile.TemporaryDirectory(prefix='orna-git-model-') as td:
  root=Path(td)
  def git(*args,input=None,index=None,ok=True):
   env=os.environ.copy();env.update(FIXED)
   if index:env['GIT_INDEX_FILE']=str(index)
   p=subprocess.run(['git',*args],cwd=root,env=env,input=input,text=True,capture_output=True)
   if ok and p.returncode:raise RuntimeError('git '+' '.join(args)+': '+p.stderr)
   return p.stdout.strip() if ok else p
  git('init','-q','-b','main');git('config','user.name','Spec fixture');git('config','user.email','spec@example.invalid')
  (root/'managed.orna').write_text('old\n');(root/'notes.txt').write_text('line one\nline two\n');git('add','.');git('commit','-qm','base');base=git('rev-parse','HEAD')
  def reset():git('reset','--hard',base);(root/'.git/index.lock').unlink(missing_ok=True)
  def private_commit(content):
   idx=root/'.git/private-index';idx.unlink(missing_ok=True);git('read-tree',base,index=idx)
   oid=git('hash-object','-w','--stdin',input=content)
   git('update-index','--add','--cacheinfo',f'100644,{oid},managed.orna',index=idx)
   tree=git('write-tree',index=idx);commit=git('commit-tree',tree,'-p',base,input='runtime batch\n');return commit,oid
  def old_bug():
   reset();new,_=private_commit('new\n');git('update-ref','HEAD',new,base)
   assert git('show',':managed.orna')=='old'
   git('commit','-qm','ordinary commit after ref-only publication')
   assert git('show','HEAD:managed.orna')=='old'
   return 'Ref-only sequence reproduces the old staged reversal with real Git.'
  check('git.original-ref-only-reversal-reproduced',old_bug)
  def reconciled():
   reset();(root/'notes.txt').write_text('staged line one\nline two\n');git('add','notes.txt')
   staged=git('show',':notes.txt');(root/'notes.txt').write_text('staged line one\nunstaged line two\n');work=(root/'notes.txt').read_text()
   new,oid=private_commit('new\n')
   reconcile=root/'.git/reconciled-index';shutil.copy2(root/'.git/index',reconcile)
   git('update-index','--cacheinfo',f'100644,{oid},managed.orna',index=reconcile)
   lock=root/'.git/index.lock';lock.write_bytes(reconcile.read_bytes())
   git('update-ref','HEAD',new,base)
   blocked=git('commit','-qm','must not race recovery',ok=False);assert blocked.returncode!=0 and 'index.lock' in blocked.stderr
   (root/'managed.orna').write_text('new\n');os.replace(lock,root/'.git/index')
   assert git('show',':notes.txt')==staged and (root/'notes.txt').read_text()==work
   assert git('diff','--cached','--','managed.orna')==''
   git('commit','-qm','human staged change')
   assert git('show','HEAD:managed.orna')=='new'
   assert git('show','HEAD:notes.txt')==staged and (root/'notes.txt').read_text()==work
   return 'Managed new value survives later commit; partial staging and unstaged bytes preserved; index lock blocks a competing commit.'
  check('git.reconciliation-preserves-partial-staging',reconciled)
  def stale():
   reset();new,_=private_commit('new\n');(root/'other.txt').write_text('concurrent\n');git('add','other.txt');git('commit','-qm','concurrent')
   head=git('rev-parse','HEAD');idx=(root/'.git/index').read_bytes()
   rejected=git('update-ref','HEAD',new,base,ok=False)
   assert rejected.returncode!=0 and git('rev-parse','HEAD')==head and (root/'.git/index').read_bytes()==idx
   return 'Expected-old ref mismatch leaves the concurrent ref and ordinary index intact.'
  check('git.ref-cas-refuses-concurrent-commit',stale)
  def branch():
   reset();(root/'notes.txt').write_text('stage\n');git('add','notes.txt');(root/'notes.txt').write_text('work\n');idx=git('write-tree')
   git('switch','-c','experiment')
   assert git('rev-parse','HEAD')==base and git('write-tree')==idx and (root/'notes.txt').read_text()=='work\n'
   return 'New branch at HEAD carries staged/unstaged changes without a commit.'
  check('git.branch-carries-changes-at-HEAD',branch)
 return results
