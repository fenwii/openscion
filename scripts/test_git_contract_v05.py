#!/usr/bin/env python3
"""Real Git oracle test for V0.5 import assumptions; does not run Rust."""
import os, pathlib, subprocess, tempfile

def git(repo,*args):
    proc=subprocess.run(['git','-C',str(repo),*args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True,
       env={**os.environ,'GIT_AUTHOR_NAME':'Oracle','GIT_AUTHOR_EMAIL':'oracle@example.invalid','GIT_COMMITTER_NAME':'Oracle','GIT_COMMITTER_EMAIL':'oracle@example.invalid'})
    return proc.stdout
with tempfile.TemporaryDirectory(prefix='openscion-v05-git-') as temp:
    repo=pathlib.Path(temp)/'repo';repo.mkdir()
    git(repo,'init','-q','-b','main')
    (repo/'a').write_text('file\n')
    git(repo,'add','-A');git(repo,'commit','-qm','initial')
    base=git(repo,'rev-parse','HEAD').strip().decode()
    git(repo,'checkout','-qb','feature')
    (repo/'a').unlink();(repo/'a').mkdir();(repo/'a'/'b').write_bytes(b'one\x00two')
    git(repo,'add','-A');git(repo,'commit','-qm','nested file')
    feature=git(repo,'rev-parse','HEAD').strip().decode()
    git(repo,'checkout','-q','main')
    (repo/'readme').write_text('doc\n')
    git(repo,'add','-A');git(repo,'commit','-qm','documentation')
    git(repo,'merge','--no-ff','-m','merge feature','feature')
    tip=git(repo,'rev-parse','HEAD').strip().decode()
    parents=git(repo,'rev-list','--parents','-n','1',tip).strip().decode().split()
    assert len(parents)==3
    tree=git(repo,'rev-parse',f'{tip}^{{tree}}').strip().decode()
    entries=git(repo,'ls-tree','-z',tree).split(b'\0')
    assert any(e.startswith(b'040000 tree ') and e.endswith(b'\ta') for e in entries)
    recursive=git(repo,'ls-tree','-rz',tip)
    assert b'\ta/b\x00' in recursive and b'\treadme\x00' in recursive
    raw=git(repo,'cat-file','commit',tip)
    assert raw.startswith(b'tree '+tree.encode()+b'\n')
    assert raw.count(b'\nparent ')==2
    assert git(repo,'rev-parse','main').strip().decode()==tip
    print('PASS: Git raw Commit parents, Tree path bytes, file-to-directory transition, and authoritative main')
