#!/usr/bin/env python3
"""Integration smoke test: real Git, API, auth, two worktrees, retention and cleanup.
Requires cargo build --workspace. Operates exclusively in a temporary directory.
"""
import json, os, pathlib, socket, subprocess, tempfile, time, urllib.request, urllib.error
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=ROOT/'target'/'debug'

def command(args, cwd=None):
    p=subprocess.run([str(x) for x in args],cwd=cwd,text=True,capture_output=True)
    if p.returncode: raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p.stdout.strip()

def request(port,token,method,path,payload=None,expect=200):
    body=None if payload is None else json.dumps(payload).encode()
    req=urllib.request.Request(f'http://127.0.0.1:{port}{path}',data=body,method=method,
        headers={'Authorization':f'Bearer {token}', 'Content-Type':'application/json'})
    try:
        with urllib.request.urlopen(req,timeout=25) as res:
            result=json.loads(res.read())
            assert res.status==expect,(res.status,result)
            return result['data']
    except urllib.error.HTTPError as e:
        raw=e.read().decode(errors='replace')
        if e.code==expect:return json.loads(raw)
        raise AssertionError(f'{method} {path}: expected {expect}, received {e.code}: {raw}')

def main():
    with tempfile.TemporaryDirectory(prefix='osc-v03-') as tmp:
        t=pathlib.Path(tmp);root=t/'git';root.mkdir();repo=root/'example';data=t/'service'
        command(['git','init','-b','main',repo]);command(['git','config','user.email','ci@example.invalid'],cwd=repo)
        command(['git','config','user.name','CI'],cwd=repo)
        (repo/'README.md').write_text('first\n'); (repo/'a.txt').write_text('alpha\n')
        command(['git','add','.'],cwd=repo);command(['git','commit','-m','baseline'],cwd=repo)
        main_before=command(['git','rev-parse','HEAD'],cwd=repo)
        token=command([BIN/'osc-admin','init','--data',data,'--allow-root',root]).split('ADMIN TOKEN (displayed once): ')[-1]
        out=command([BIN/'osc-admin','register','--data',data,'--name','example','--path',repo]);repo_id=out.split('id=')[1].split()[0]
        agent_token=command([BIN/'osc-admin','create-token','--data',data,'--principal','agent01','--role','writer']).split('TOKEN for agent01 (displayed once): ')[-1]
        command([BIN/'osc-admin','grant','--data',data,'--repo','example','--principal','agent01'])
        for perm in ['create','commit','dispose','renew']:
            command([BIN/'osc-admin','grant-workspace','--data',data,'--repo','example','--principal','agent01','--permission',perm])
        denied_token=command([BIN/'osc-admin','create-token','--data',data,'--principal','read01','--role','reader']).split('TOKEN for read01 (displayed once): ')[-1]
        command([BIN/'osc-admin','grant','--data',data,'--repo','example','--principal','read01'])
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        server=subprocess.Popen([str(BIN/'osc-server'),'--data',str(data),'--bind',f'127.0.0.1:{port}'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        try:
            for _ in range(100):
                if server.poll() is not None:raise RuntimeError(f'server exited: {server.stderr.read()}')
                try: urllib.request.urlopen(f'http://127.0.0.1:{port}/health/live',timeout=.2);break
                except Exception:time.sleep(.08)
            else:raise RuntimeError('server did not start')
            base=request(port,token,'GET',f'/v1/repos/{repo_id}/refs/resolve?name=HEAD')['resolved_oid']
            assert base.endswith(main_before)
            template={'repository_id':repo_id,'base_revision':base,'lease_seconds':3600}
            request(port,denied_token,'POST','/v1/workspaces',{**template,'idempotency_key':'unauthorized'},expect=403)
            w1=request(port,agent_token,'POST','/v1/workspaces',{**template,'idempotency_key':'create1','task_id':'task1'},expect=201)
            assert request(port,agent_token,'POST','/v1/workspaces',{**template,'idempotency_key':'create1','task_id':'task1'},expect=201)['id']==w1['id']
            w2=request(port,agent_token,'POST','/v1/workspaces',{**template,'idempotency_key':'create2','task_id':'task2'},expect=201)
            path1=data/'workspaces'/repo_id/w1['id'];path2=data/'workspaces'/repo_id/w2['id'];assert path1.is_dir() and path2.is_dir()
            (path1/'a.txt').write_text('agent1\n');(path2/'a.txt').write_text('agent2\n')
            s1=request(port,agent_token,'GET',f'/v1/workspaces/{w1["id"]}/status');assert s1['dirty']
            for w,n in [(w1,1),(w2,2)]:
                body={'expected_head':base,'message':f'agent {n}', 'selected_paths':['a.txt'],'idempotency_key':f'commit{n}'}
                result=request(port,agent_token,'POST',f'/v1/workspaces/{w["id"]}/commits',body)
                assert result['retained'] and not result['merged']
                again=request(port,agent_token,'POST',f'/v1/workspaces/{w["id"]}/commits',body)
                assert again==result
                ref=command(['git','show-ref','--hash',f'refs/openscion/workspaces/{w["id"]}'],cwd=repo)
                assert ref==result['commit_oid']
                request(port,agent_token,'DELETE',f'/v1/workspaces/{w["id"]}')
            assert command(['git','rev-parse','main'],cwd=repo)==main_before
            assert not path1.exists() and not path2.exists()
            print('PASS: auth, two independent worktrees, idempotent commits, retention, disposal, protected main')
        finally:
            server.terminate()
            try:server.communicate(timeout=5)
            except subprocess.TimeoutExpired:server.kill();server.communicate()

if __name__=='__main__':main()
