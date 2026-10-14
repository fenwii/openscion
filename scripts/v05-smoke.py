#!/usr/bin/env python3
"""V0.5 live HTTP smoke; requires built Rust binaries in target/debug."""
import json, os, pathlib, re, socket, subprocess, tempfile, time, urllib.error, urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=ROOT/'target'/'debug'
def run(*args,cwd=None):
    proc=subprocess.run([str(x) for x in args],cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,timeout=120)
    if proc.returncode:raise RuntimeError(f'command {args} failed: {proc.stderr}')
    return proc.stdout.strip()
def api(base,token,method,path,payload=None,expected=200):
    url=base+path
    req=urllib.request.Request(url,method=method,headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},
       data=None if payload is None else json.dumps(payload).encode())
    try:
        with urllib.request.urlopen(req,timeout=120) as resp:
            assert resp.status==expected,(resp.status,expected)
            return json.loads(resp.read())['data']
    except urllib.error.HTTPError as err:
        if err.code!=expected:raise AssertionError(f'{path}: expected {expected}, received {err.code}: {err.read()!r}')
        return json.loads(err.read())
with tempfile.TemporaryDirectory(prefix='openscion-v05-http-') as temp:
    root=pathlib.Path(temp);allowed=root/'allowed';allowed.mkdir();repo=allowed/'repo';data=root/'data'
    run('git','init','-b','main',repo)
    run('git','config','user.name','Smoke',cwd=repo);run('git','config','user.email','smoke@example.invalid',cwd=repo)
    (repo/'item').write_text('initial')
    run('git','add','-A',cwd=repo);run('git','commit','-m','root',cwd=repo)
    first=run('git','rev-parse','HEAD',cwd=repo)
    (repo/'item').unlink();(repo/'item').mkdir();(repo/'item'/'child').write_bytes(b'child\x00data')
    run('git','add','-A',cwd=repo);run('git','commit','-m','nested',cwd=repo)
    second=run('git','rev-parse','HEAD',cwd=repo)
    admin_output=run(BIN/'osc-admin','init','--data',data,'--allow-root',allowed)
    admin=re.search(r'osc_[a-f0-9]+',admin_output).group()
    registered=run(BIN/'osc-admin','register','--data',data,'--name','repo','--path',repo)
    repo_id=re.search(r'id=([a-f0-9-]+)',registered).group(1)
    viewer_output=run(BIN/'osc-admin','create-token','--data',data,'--principal','reader','--role','reader')
    viewer=re.search(r'osc_[a-f0-9]+',viewer_output).group()
    run(BIN/'osc-admin','grant','--data',data,'--repo','repo','--principal','reader')
    with socket.socket() as sk: sk.bind(('127.0.0.1',0));port=sk.getsockname()[1]
    proc=subprocess.Popen([str(BIN/'osc-server'),'--data',str(data),'--bind',f'127.0.0.1:{port}'],
                          stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    base=f'http://127.0.0.1:{port}'
    try:
        for _ in range(80):
            if proc.poll() is not None:raise RuntimeError('server exited unexpectedly')
            try:
                urllib.request.urlopen(base+'/health/live',timeout=.2);break
            except Exception:time.sleep(.15)
        else:raise RuntimeError('server startup timeout')
        route=f'/v1/repos/{repo_id}/shadow'
        assert not api(base,viewer,'GET',route)['enabled']
        denied=api(base,viewer,'POST',route+'/enable',{'tracked_ref':'refs/heads/main'},403)
        assert denied['error']['code']=='PERMISSION_DENIED'
        enabled=api(base,admin,'POST',route+'/enable',{'tracked_ref':'refs/heads/main'})
        assert enabled['enabled']
        results=api(base,admin,'POST',route+'/work',{'max_jobs':1})
        assert len(results)==1 and results[0]['state']=='VERIFIED',results
        mapped=api(base,viewer,'GET',route+'/mappings/git:sha1:'+second)
        assert mapped['native_id'].startswith('osc:changeset:sha256:')
        assert mapped['state']=='VERIFIED'
        assert api(base,viewer,'GET',route+'/mappings/git:sha1:'+first)['state']=='VERIFIED'
        assert len(api(base,viewer,'GET',route+'/verify?limit=10'))>=2
        assert run('git','rev-parse','main',cwd=repo)==second
        print('PASS: V0.5 HTTP auth, shadow import, Git/native mapping, parity, preserved Git authority')
    finally:
        proc.terminate()
        try:proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:proc.kill();proc.communicate()
