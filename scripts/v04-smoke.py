#!/usr/bin/env python3
"""V0.4 HTTP smoke: auth -> workspace commit -> preview -> validate -> protected-ref CAS.
Requires `cargo build --workspace`; always uses temporary repository and service.
"""
import json, os, pathlib, socket, subprocess, tempfile, time, urllib.error, urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[1];BIN=ROOT/'target'/'debug'
def run(*argv,cwd=None):
    p=subprocess.run([str(x) for x in argv],cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,timeout=90)
    if p.returncode:raise RuntimeError(f'{argv!r}: {p.stderr}')
    return p.stdout.strip()
def api(port,token,method,path,payload=None,status=200):
    req=urllib.request.Request(f'http://127.0.0.1:{port}{path}',
       method=method,data=None if payload is None else json.dumps(payload).encode(),
       headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
    try:
        with urllib.request.urlopen(req,timeout=90) as res:
            v=json.loads(res.read());assert res.status==status,(res.status,v)
            return v['data']
    except urllib.error.HTTPError as e:
        if e.code==status:return json.loads(e.read())
        raise AssertionError(f'{method} {path}: expected {status}, got {e.code}, {e.read()}')
def main():
    with tempfile.TemporaryDirectory(prefix='openscion-v04-') as tmp:
        root=pathlib.Path(tmp);allowed=root/'allowed';allowed.mkdir();repo=allowed/'repo';data=root/'service'
        run('git','init','-b','main',repo)
        run('git','config','user.name','Demo',cwd=repo);run('git','config','user.email','demo@example.invalid',cwd=repo)
        (repo/'readme.txt').write_text('baseline\n')
        run('git','add','.',cwd=repo);run('git','commit','-m','baseline',cwd=repo)
        before=run('git','rev-parse','main',cwd=repo)
        token=run(BIN/'osc-admin','init','--data',data,'--allow-root',allowed).split('ADMIN TOKEN (displayed once): ')[-1]
        out=run(BIN/'osc-admin','register','--data',data,'--name','demo','--path',repo)
        rid=out.split('id=')[1].split()[0]
        run(BIN/'osc-admin','policy-install','--data',data,'--repo','demo','--target','refs/heads/main','--approvals','0')
        agent=run(BIN/'osc-admin','create-token','--data',data,'--principal','agent','--role','writer').split('TOKEN for agent (displayed once): ')[-1]
        viewer=run(BIN/'osc-admin','create-token','--data',data,'--principal','viewer','--role','reader').split('TOKEN for viewer (displayed once): ')[-1]
        for principal in ['agent','viewer']:
            run(BIN/'osc-admin','grant','--data',data,'--repo','demo','--principal',principal)
        for perm in ['create','commit']:
            run(BIN/'osc-admin','grant-workspace','--data',data,'--repo','demo','--principal','agent','--permission',perm)
        for perm in ['submit','preview','validate','land']:
            run(BIN/'osc-admin','grant-change','--data',data,'--repo','demo','--principal','agent','--permission',perm)
        with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        server=subprocess.Popen([str(BIN/'osc-server'),'--data',str(data),'--bind',f'127.0.0.1:{port}'],stderr=subprocess.PIPE)
        try:
            for i in range(100):
                if server.poll() is not None:raise RuntimeError('server exited early')
                try:urllib.request.urlopen(f'http://127.0.0.1:{port}/health/live',timeout=.2);break
                except Exception:time.sleep(.1)
            else:raise RuntimeError('server timed out')
            ws=api(port,agent,'POST','/v1/workspaces',{
                'repository_id':rid,'base_revision':'refs/heads/main','task_id':'T-001','lease_seconds':3600,
                'idempotency_key':'create-t-001'},status=201)
            workdir=data/'workspaces'/rid/ws['id']
            assert workdir.is_dir()
            (workdir/'new.txt').write_text('agent change\n')
            committed=api(port,agent,'POST',f"/v1/workspaces/{ws['id']}/commits",{
                'expected_head':ws['head_oid'],'message':'new feature','selected_paths':['new.txt'],
                'idempotency_key':'commit-t-001'})
            assert committed['retained']
            change_payload={'repository_id':rid,'source_workspace_id':ws['id'],
                'source_commit_oid':committed['commit_oid'],'target_ref':'refs/heads/main',
                'title':'new feature','description':'smoke test','idempotency_key':'submit-t-001'}
            denied=api(port,viewer,'POST','/v1/change-requests',change_payload,status=403)
            assert denied['error']['code']=='PERMISSION_DENIED'
            change=api(port,agent,'POST','/v1/change-requests',change_payload,status=201)
            assert api(port,agent,'POST','/v1/change-requests',change_payload,status=201)['id']==change['id']
            operation=api(port,agent,'POST',f"/v1/change-requests/{change['id']}/preview",{'idempotency_key':'preview-t-001'})
            assert operation['state']=='CANDIDATE_READY',operation
            assert run('git','rev-parse','main',cwd=repo)==before
            api(port,agent,'POST',f"/v1/land-operations/{operation['id']}/land",{},status=409)
            validated=api(port,agent,'POST',f"/v1/land-operations/{operation['id']}/validate",{})
            assert validated['state']=='VALIDATED'
            merged=api(port,agent,'POST',f"/v1/land-operations/{operation['id']}/land",{})
            assert merged['state']=='MERGED'
            assert run('git','rev-parse','main',cwd=repo)==merged['candidate_oid']
            assert run('git','show','main:new.txt',cwd=repo)=='agent change'
            assert api(port,agent,'POST',f"/v1/land-operations/{operation['id']}/land",{})['state']=='MERGED'
            print('PASS: V0.4 HTTP auth, submission, preview, validation gate, CAS, idempotency and Git authority')
        finally:
            server.terminate()
            try:server.communicate(timeout=5)
            except subprocess.TimeoutExpired:server.kill();server.communicate()
if __name__=='__main__':main()
