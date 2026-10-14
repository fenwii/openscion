#!/usr/bin/env python3
"""Runs against built osc-admin + osc-server; uses real Git fixture and HTTP auth."""
import base64, json, os, pathlib, re, socket, subprocess, tempfile, time, urllib.request, urllib.error
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=ROOT/'target'/'debug'

def run(*args,cwd=None):
    p=subprocess.run(args,cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,check=True)
    return p.stdout.strip()

def url_fetch(url,token=None,expect=200):
    headers={'Authorization':'Bearer '+token} if token else {}
    try:
        with urllib.request.urlopen(urllib.request.Request(url,headers=headers),timeout=3) as resp:
            status=resp.status; content=resp.read()
    except urllib.error.HTTPError as exc:
        status=exc.code; content=exc.read()
    assert status==expect,(url,status,expect,content[:1000]);return content

with tempfile.TemporaryDirectory() as temp:
    t=pathlib.Path(temp);repo=t/'repository';repo.mkdir();data=t/'service'
    run('git','init','-q',str(repo));run('git','config','user.email','test@example.com',cwd=repo);run('git','config','user.name','Test',cwd=repo)
    (repo/'source.bin').write_bytes(b'zero\x00one\n')
    run('git','add','source.bin',cwd=repo);run('git','commit','-qm','baseline',cwd=repo)
    old=run('git','rev-parse','HEAD',cwd=repo)
    (repo/'source.bin').write_bytes(b'changed\x00again\n')
    run('git','add','source.bin',cwd=repo);run('git','commit','-qm','next',cwd=repo)
    head=run('git','rev-parse','HEAD',cwd=repo)
    result=run(str(BIN/'osc-admin'),'init','--data',str(data),'--allow-root',str(t))
    token=re.search(r'osc_[a-f0-9]+',result).group(0)
    record=run(str(BIN/'osc-admin'),'register','--data',str(data),'--name','fixture','--path',str(repo))
    repo_id=re.search(r'id=([a-f0-9-]+)',record).group(1)
    with socket.socket() as s:
        s.bind(('127.0.0.1',0));port=s.getsockname()[1]
    server=subprocess.Popen([str(BIN/'osc-server'),'--data',str(data),'--bind',f'127.0.0.1:{port}'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    base=f'http://127.0.0.1:{port}'
    try:
        for i in range(40):
            try: url_fetch(base+'/health/live');break
            except Exception:time.sleep(.1)
        else:raise RuntimeError('server failed to start: '+str(server.poll()))
        url_fetch(base+'/v1/repos',expect=401)
        repos=json.loads(url_fetch(base+'/v1/repos',token))['data']; assert len(repos)==1
        rev=json.loads(url_fetch(base+f'/v1/repos/{repo_id}/refs/resolve?name=HEAD',token))['data']['resolved_oid'];assert rev.endswith(head)
        commit=json.loads(url_fetch(base+f'/v1/repos/{repo_id}/commits/{rev}',token))['data'];assert len(commit['parents'])==1
        path=base64.urlsafe_b64encode(b'source.bin').rstrip(b'=').decode()
        content=url_fetch(base+f'/v1/repos/{repo_id}/files/{rev}?path_b64={path}',token)
        assert content==(repo/'source.bin').read_bytes()
        tree=json.loads(url_fetch(base+f'/v1/repos/{repo_id}/trees/{rev}',token))['data'];assert len(tree['entries'])==1
        changed=json.loads(url_fetch(base+f'/v1/repos/{repo_id}/changed-paths?base={old}&target={head}',token))['data'];assert len(changed)==1
        bases=json.loads(url_fetch(base+f'/v1/repos/{repo_id}/merge-bases?left={old}&right={head}',token))['data'];assert bases[0].endswith(old)
        hist=json.loads(url_fetch(base+f'/v1/repos/{repo_id}/history?start={head}&limit=10',token))['data'];assert len(hist['commits'])==2
        print('PASS: authentication, refs, commits, trees, binary files, changed paths, merge bases, history')
    finally:
        server.terminate()
        try:server.wait(timeout=5)
        except subprocess.TimeoutExpired:server.kill();server.wait()
