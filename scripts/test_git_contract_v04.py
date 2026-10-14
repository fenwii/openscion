#!/usr/bin/env python3
"""Real Git plumbing contract test; not a Rust integration test."""
import os, pathlib, subprocess, tempfile

def run(repo,*args,input=None,allow_conflict=False):
    env=os.environ.copy();env.update({'GIT_AUTHOR_NAME':'Test','GIT_AUTHOR_EMAIL':'test@example.invalid',
       'GIT_COMMITTER_NAME':'Test','GIT_COMMITTER_EMAIL':'test@example.invalid',
       'GIT_CONFIG_NOSYSTEM':'1','GIT_NO_REPLACE_OBJECTS':'1','GIT_NO_LAZY_FETCH':'1'})
    p=subprocess.run(['git','-C',str(repo),*args],input=input,env=env,capture_output=True)
    assert p.returncode==0 or (allow_conflict and p.returncode==1),(args,p.returncode,p.stderr.decode(errors='replace'))
    return p.returncode,p.stdout

def cmd(repo,*args):return run(repo,*args)[1].decode().strip()

with tempfile.TemporaryDirectory(prefix='osc-v04-git-') as tmp:
    repo=pathlib.Path(tmp)/'repo';repo.mkdir();cmd(repo,'init','--initial-branch=main')
    (repo/'shared.txt').write_text('shared base\n');cmd(repo,'add','shared.txt');cmd(repo,'commit','-m','initial')
    base=cmd(repo,'rev-parse','main')
    cmd(repo,'checkout','-b','agent')
    (repo/'feature.txt').write_text('agent content\n');cmd(repo,'add','feature.txt');cmd(repo,'commit','-m','agent')
    source=cmd(repo,'rev-parse','HEAD')
    cmd(repo,'checkout','main')
    (repo/'main.txt').write_text('main content\n');cmd(repo,'add','main.txt');cmd(repo,'commit','-m','main')
    tip=cmd(repo,'rev-parse','main')
    rc,data=run(repo,'merge-tree','--write-tree','-z',tip,source)
    assert rc==0
    tree=data.split(b'\0',1)[0].decode()
    assert len(tree)==len(base)
    candidate=run(repo,'commit-tree',tree,'-p',tip,input=b'Integrate V0.4 contract\n')[1].decode().strip()
    assert cmd(repo,'show','-s','--format=%T',candidate)==tree
    assert cmd(repo,'rev-parse',candidate+'^1')==tip
    retain='refs/openscion/land/git-contract-1'
    cmd(repo,'update-ref',retain,candidate,'0'*len(candidate))
    assert cmd(repo,'rev-parse',retain)==candidate
    assert cmd(repo,'rev-parse','main')==tip, 'preview must not change main'
    cmd(repo,'update-ref','--no-deref','refs/heads/main',candidate,tip)
    assert cmd(repo,'rev-parse','main')==candidate
    # Competing old-old update must fail (simulates optimistic concurrency).
    p=subprocess.run(['git','-C',str(repo),'update-ref','refs/heads/main',source,tip],capture_output=True)
    assert p.returncode!=0
    # Conflict preview: branches modify the same original line.
    cmd(repo,'checkout','-b','conflict-A',base)
    (repo/'shared.txt').write_text('changed A\n');cmd(repo,'add','shared.txt');cmd(repo,'commit','-m','A');a=cmd(repo,'rev-parse','HEAD')
    cmd(repo,'checkout','-b','conflict-B',base)
    (repo/'shared.txt').write_text('changed B\n');cmd(repo,'add','shared.txt');cmd(repo,'commit','-m','B');b=cmd(repo,'rev-parse','HEAD')
    rc,_=run(repo,'merge-tree','--write-tree','-z',a,b,allow_conflict=True)
    assert rc==1
    print('PASS: Git merge-tree clean/conflict, candidate commit parent/tree, retention ref and CAS')
