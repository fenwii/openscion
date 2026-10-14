#!/usr/bin/env python3
"""Real local Git mirror and protected CAS contract; tests Git, not Rust."""
from pathlib import Path
import subprocess,tempfile

def git(repo,*args,success=True):
    result=subprocess.run(['git','-C',str(repo),*args],capture_output=True,text=True,timeout=30)
    if success and result.returncode:raise RuntimeError(f'git {args!r}: {result.stderr}')
    return result.stdout.strip() if success else result
with tempfile.TemporaryDirectory(prefix='openscion-v06-git-') as tmp:
    repo=Path(tmp)/'repo'
    repo.mkdir()
    git(repo,'init','-b','main')
    git(repo,'config','user.name','OpenScion Test')
    git(repo,'config','user.email','test@openscion.invalid')
    (repo/'README.md').write_text('initial\n')
    git(repo,'add','README.md');git(repo,'commit','-m','initial')
    a=git(repo,'rev-parse','refs/heads/main')
    git(repo,'checkout','-b','feature')
    (repo/'README.md').write_text('feature\n')
    git(repo,'add','README.md');git(repo,'commit','-m','feature')
    b=git(repo,'rev-parse','HEAD')
    git(repo,'checkout','main')
    assert git(repo,'rev-parse','main')==a
    zero='0'*len(a)
    git(repo,'update-ref','--no-deref','refs/openscion/mirror/main',b,zero)
    assert git(repo,'rev-parse','refs/openscion/mirror/main')==b
    assert git(repo,'rev-parse','main')==a,'mirror must not modify main'
    bad=git(repo,'update-ref','--no-deref','refs/heads/main',b,zero,success=False)
    assert bad.returncode!=0,'Git CAS unexpectedly accepted an incorrect expected OID'
    assert git(repo,'rev-parse','main')==a
    # Reconciliation after fallback Git CAS, before SQLite finalization:
    git(repo,'update-ref','--no-deref','refs/heads/main',b,a)
    assert git(repo,'rev-parse','main')==b
    print('PASS: mirror-only Ref update, protected Git CAS rejection, safe accepted fallback target')
