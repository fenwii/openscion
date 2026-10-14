/** OpenScion V0.2 source API client. All content reads are pinned to exact Git revisions. */
export type GitOid = `git:${"sha1" | "sha256"}:${string}`;
export interface Repository { id: string; name: string; kind: string; object_format: "sha1" | "sha256" }
export interface TreeEntry { name_b64: string; name_display: string; mode: string; kind: string; oid: GitOid }
export interface Commit {oid:GitOid;tree_oid:GitOid;parents:GitOid[];message_b64:string;message_display:string;raw_b64:string}
export interface ChangedPath {path_b64:string;path_display:string;status:string;old_mode:string;new_mode:string;old_oid:string;new_oid:string}
export interface HistoryPage {start:GitOid;skip:number;limit:number;commits:GitOid[];next_skip:number|null;shallow_repository:boolean}
export class OpenScionError extends Error {
  constructor(public readonly code:string, public readonly status:number, message:string) {super(message); this.name="OpenScionError";}
}
function pathB64(bytes: Uint8Array): string {
  // Browser-friendly base64url conversion; chunking avoids call-stack limits.
  let binary="";for (let i=0;i<bytes.length;i+=4096) binary+=String.fromCharCode(...bytes.subarray(i,i+4096));
  return btoa(binary).replace(/\+/g,"-").replace(/\//g,"_").replace(/=/g,"");
}
export class OpenScionClient {
  constructor(private readonly baseUrl:string, private readonly token:string) {}
  private async request(path:string,signal?:AbortSignal):Promise<Response> {
    const res=await fetch(`${this.baseUrl.replace(/\/$/,"")}${path}`,{
      headers:{Authorization:`Bearer ${this.token}`},signal
    });
    if (!res.ok) {
      const payload: {error?:{code?:string;message?:string}} = await res.json().catch(()=>({}));
      throw new OpenScionError(payload.error?.code??"HTTP_ERROR",res.status,payload.error?.message??`HTTP ${res.status}`);
    }
    return res;
  }
  private async json<T>(path:string,signal?:AbortSignal):Promise<T> {
    const response=await this.request(path,signal);
    const envelope=await response.json() as {data:T};
    return envelope.data;
  }
  listRepositories(signal?:AbortSignal):Promise<Repository[]> {return this.json("/v1/repos",signal);}
  resolveRef(repoId:string,name:string,signal?:AbortSignal):Promise<{resolved_oid:GitOid}> {
    return this.json(`/v1/repos/${encodeURIComponent(repoId)}/refs/resolve?name=${encodeURIComponent(name)}`,signal);
  }
  getCommit(repoId:string,oid:GitOid,signal?:AbortSignal):Promise<Commit> {
    return this.json(`/v1/repos/${encodeURIComponent(repoId)}/commits/${encodeURIComponent(oid)}`,signal);
  }
  listTree(repoId:string,oid:GitOid,path:Uint8Array=new Uint8Array(),signal?:AbortSignal):Promise<{tree_oid:GitOid;entries:TreeEntry[]}> {
    return this.json(`/v1/repos/${encodeURIComponent(repoId)}/trees/${encodeURIComponent(oid)}?path_b64=${pathB64(path)}`,signal);
  }
  async getFile(repoId:string,oid:GitOid,path:Uint8Array,signal?:AbortSignal):Promise<Uint8Array> {
    const res=await this.request(`/v1/repos/${encodeURIComponent(repoId)}/files/${encodeURIComponent(oid)}?path_b64=${pathB64(path)}`,signal);
    return new Uint8Array(await res.arrayBuffer());
  }
  async getDiff(repoId:string,base:GitOid,target:GitOid,signal?:AbortSignal):Promise<Uint8Array> {
    const res=await this.request(`/v1/repos/${encodeURIComponent(repoId)}/diff?base=${encodeURIComponent(base)}&target=${encodeURIComponent(target)}`,signal);
    return new Uint8Array(await res.arrayBuffer());
  }
  getChangedPaths(repoId:string,base:GitOid,target:GitOid,signal?:AbortSignal):Promise<ChangedPath[]> {
    return this.json(`/v1/repos/${encodeURIComponent(repoId)}/changed-paths?base=${encodeURIComponent(base)}&target=${encodeURIComponent(target)}`,signal);
  }
  getMergeBases(repoId:string,left:GitOid,right:GitOid,signal?:AbortSignal):Promise<GitOid[]> {
    return this.json(`/v1/repos/${encodeURIComponent(repoId)}/merge-bases?left=${encodeURIComponent(left)}&right=${encodeURIComponent(right)}`,signal);
  }
  getHistory(repoId:string,start:GitOid,options:{limit?:number;skip?:number;firstParent?:boolean}={},signal?:AbortSignal):Promise<HistoryPage> {
    const q=new URLSearchParams({start,limit:String(options.limit??50),skip:String(options.skip??0),first_parent:String(options.firstParent??false)});
    return this.json(`/v1/repos/${encodeURIComponent(repoId)}/history?${q}`,signal);
  }
}

/** V0.3: workspaces are Git worktrees, not execution sandboxes. */
export interface Workspace {
  id: string;
  repo_id: string;
  owner: string;
  task_id: string|null;
  base_oid: GitOid;
  head_oid: GitOid;
  state: string;
  generation: number;
  lease_expires: number;
  created_at: number;
  updated_at: number;
  error: string|null;
}
export interface WorkspaceStatusEntry {
  path_b64: string;
  index_status: string;
  worktree_status: string;
  original_path_b64: string|null;
}
export interface WorkspaceStatus {
  workspace_id: string;
  base_oid: GitOid;
  head_oid: string;
  dirty: boolean;
  entries: WorkspaceStatusEntry[];
}
export interface WorkspaceCommit {
  workspace_id: string;
  commit_oid: string;
  parent_oid: string;
  retained: boolean;
  merged: false;
}
export class WorkspaceClient extends OpenScionClient {
  constructor(private readonly endpoint: string, private readonly bearer: string) { super(endpoint,bearer); }
  private async call<T>(path:string, method:string="GET",body?:unknown,signal?:AbortSignal):Promise<T>{
    const res=await fetch(`${this.endpoint.replace(/\/$/,"")}${path}`,{
      method,headers:{Authorization:`Bearer ${this.bearer}`,...(body===undefined?{}:{"Content-Type":"application/json"})},
      body:body===undefined?undefined:JSON.stringify(body),signal
    });
    if(!res.ok){
      const payload:{error?:{code?:string;message?:string}}=await res.json().catch(()=>({}));
      throw new OpenScionError(payload.error?.code??"HTTP_ERROR",res.status,payload.error?.message??`HTTP ${res.status}`);
    }
    return (await res.json() as {data:T}).data;
  }
  createWorkspace(request:{repository_id:string;base_revision:GitOid|string;task_id?:string;lease_seconds?:number;idempotency_key:string},signal?:AbortSignal):Promise<Workspace>{
    return this.call("/v1/workspaces","POST",request,signal);
  }
  listWorkspaces(signal?:AbortSignal):Promise<Workspace[]>{return this.call("/v1/workspaces","GET",undefined,signal);}
  getWorkspace(id:string,signal?:AbortSignal):Promise<Workspace>{return this.call(`/v1/workspaces/${encodeURIComponent(id)}`,"GET",undefined,signal);}
  getWorkspaceStatus(id:string,signal?:AbortSignal):Promise<WorkspaceStatus>{return this.call(`/v1/workspaces/${encodeURIComponent(id)}/status`,"GET",undefined,signal);}
  commitWorkspace(id:string,request:{expected_head:string;message:string;selected_paths:string[];idempotency_key:string},signal?:AbortSignal):Promise<WorkspaceCommit>{
    return this.call(`/v1/workspaces/${encodeURIComponent(id)}/commits`,"POST",request,signal);
  }
  renewWorkspace(id:string,leaseSeconds:number,signal?:AbortSignal):Promise<Workspace>{
    return this.call(`/v1/workspaces/${encodeURIComponent(id)}/lease`,"POST",{lease_seconds:leaseSeconds},signal);
  }
  disposeWorkspace(id:string,signal?:AbortSignal):Promise<Workspace>{
    return this.call(`/v1/workspaces/${encodeURIComponent(id)}`,"DELETE",undefined,signal);
  }
}

/** V0.4: Git remains authoritative. Preview, verify and land exact candidate commits. */
export interface ChangeRequest {
  id: string; repo_id: string; workspace_id: string; source_oid: string;
  base_oid: string; target_ref: string; author: string; title: string;
  description: string; state: string; revision: number; generation: number;
}
export interface LandOperation {
  id: string; change_id: string; actor: string; state: string;
  attempt: number; result_oid: string|null; error_code: string|null;
  observed_target: string|null; candidate_tree: string|null;
  candidate_oid: string|null; policy_digest: string|null;
}
export interface SubmitChangeRequest {
  repository_id:string; source_workspace_id:string; source_commit_oid:string;
  target_ref:string; title:string; description?:string; idempotency_key:string;
}
export class ChangeClient {
  constructor(private readonly endpoint:string,private readonly bearer:string) {}
  private async request<T>(path:string,method:string="GET",body?:unknown,signal?:AbortSignal):Promise<T>{
    const res=await fetch(`${this.endpoint.replace(/\/$/,"")}${path}`,{
      method,headers:{Authorization:`Bearer ${this.bearer}`,...(body===undefined?{}:{"Content-Type":"application/json"})},
      body:body===undefined?undefined:JSON.stringify(body),signal
    });
    if(!res.ok){
      const payload:{error?:{code?:string;message?:string}}=await res.json().catch(()=>({}));
      throw new OpenScionError(payload.error?.code??"HTTP_ERROR",res.status,payload.error?.message??`HTTP ${res.status}`);
    }
    return (await res.json() as {data:T}).data;
  }
  submitChange(request:SubmitChangeRequest,signal?:AbortSignal):Promise<ChangeRequest>{
    return this.request("/v1/change-requests","POST",request,signal);
  }
  listChanges(signal?:AbortSignal):Promise<ChangeRequest[]>{return this.request("/v1/change-requests","GET",undefined,signal);}
  getChange(id:string,signal?:AbortSignal):Promise<ChangeRequest>{return this.request(`/v1/change-requests/${encodeURIComponent(id)}`,"GET",undefined,signal);}
  previewChange(id:string,key:string,signal?:AbortSignal):Promise<LandOperation>{
    return this.request(`/v1/change-requests/${encodeURIComponent(id)}/preview`,"POST",{idempotency_key:key},signal);
  }
  getLandOperation(id:string,signal?:AbortSignal):Promise<LandOperation>{return this.request(`/v1/land-operations/${encodeURIComponent(id)}`,"GET",undefined,signal);}
  validateCandidate(id:string,signal?:AbortSignal):Promise<LandOperation>{return this.request(`/v1/land-operations/${encodeURIComponent(id)}/validate`,"POST",{},signal);}
  reviewCandidate(id:string,decision:"APPROVE"|"REJECT",signal?:AbortSignal):Promise<LandOperation>{
    return this.request(`/v1/land-operations/${encodeURIComponent(id)}/reviews`,"POST",{decision},signal);
  }
  attestCandidate(id:string,proof:{check_name:string;candidate_oid:string;tree_oid:string;suite_digest:string;runner_digest:string;passed:boolean},signal?:AbortSignal):Promise<LandOperation>{
    return this.request(`/v1/land-operations/${encodeURIComponent(id)}/attest`,"POST",proof,signal);
  }
  landChange(id:string,signal?:AbortSignal):Promise<LandOperation>{return this.request(`/v1/land-operations/${encodeURIComponent(id)}/land`,"POST",{},signal);}
  reconcile(id:string,signal?:AbortSignal):Promise<LandOperation>{return this.request(`/v1/land-operations/${encodeURIComponent(id)}/reconcile`,"POST",{},signal);}
  repreview(id:string,signal?:AbortSignal):Promise<LandOperation>{return this.request(`/v1/land-operations/${encodeURIComponent(id)}/repreview`,"POST",{},signal);}
}

/** V0.5: non-authoritative Git/native shadow state and verification. */
export interface ShadowStatus {
  repo_id:string;enabled:boolean;tracked_ref:string;state:string;
  last_observed_oid:string|null;verified_mappings:number;pending_jobs:number;failed_jobs:number;
}
export interface ShadowMapping {
  repo_id:string;git_oid:string;native_id:string;root_tree_id:string;
  git_tree_oid:string;manifest_digest:string;state:"VERIFIED"|"MISMATCH"|"INELIGIBLE"|"BLOCKED";
}
export interface ShadowJob {
  id:string;repo_id:string;git_oid:string;state:string;
  generation:number;attempts:number;last_error:string|null;
}
export interface ShadowWorkerResult {job_id:string;state:string;result:string|null;error:string|null}
export class ShadowClient {
  constructor(private readonly endpoint:string,private readonly bearer:string) {}
  private async request<T>(path:string,method:string="GET",body?:unknown,signal?:AbortSignal):Promise<T>{
    const res=await fetch(`${this.endpoint.replace(/\/$/,"")}${path}`,{
      method,headers:{Authorization:`Bearer ${this.bearer}`,...(body===undefined?{}:{"Content-Type":"application/json"})},
      body:body===undefined?undefined:JSON.stringify(body),signal
    });
    if(!res.ok){
      const payload:{error?:{code?:string;message?:string}}=await res.json().catch(()=>({}));
      throw new OpenScionError(payload.error?.code??"HTTP_ERROR",res.status,payload.error?.message??`HTTP ${res.status}`);
    }
    return (await res.json() as {data:T}).data;
  }
  private prefix(repoId:string):string{return `/v1/repos/${encodeURIComponent(repoId)}/shadow`;}
  status(repoId:string,signal?:AbortSignal):Promise<ShadowStatus>{return this.request(this.prefix(repoId),"GET",undefined,signal);}
  mapping(repoId:string,oid:GitOid,signal?:AbortSignal):Promise<ShadowMapping>{
    return this.request(`${this.prefix(repoId)}/mappings/${encodeURIComponent(oid)}`,"GET",undefined,signal);
  }
  jobs(repoId:string,signal?:AbortSignal):Promise<ShadowJob[]>{return this.request(`${this.prefix(repoId)}/jobs`,"GET",undefined,signal);}
  verify(repoId:string,limit:number=10,signal?:AbortSignal):Promise<ShadowMapping[]>{
    return this.request(`${this.prefix(repoId)}/verify?limit=${limit}`,"GET",undefined,signal);
  }
  enable(repoId:string,trackedRef:string,signal?:AbortSignal):Promise<ShadowStatus>{
    return this.request(`${this.prefix(repoId)}/enable`,"POST",{tracked_ref:trackedRef},signal);
  }
  observe(repoId:string,signal?:AbortSignal):Promise<ShadowJob>{return this.request(`${this.prefix(repoId)}/observe`,"POST",{},signal);}
  enqueue(repoId:string,gitOid:GitOid,signal?:AbortSignal):Promise<ShadowJob>{
    return this.request(`${this.prefix(repoId)}/enqueue`,"POST",{git_oid:gitOid},signal);
  }
  work(repoId:string,maxJobs:number=1,signal?:AbortSignal):Promise<ShadowWorkerResult[]>{
    return this.request(`${this.prefix(repoId)}/work`,"POST",{max_jobs:maxJobs},signal);
  }
  retry(repoId:string,jobId:string,signal?:AbortSignal):Promise<ShadowJob>{
    return this.request(`${this.prefix(repoId)}/jobs/${encodeURIComponent(jobId)}/retry`,"POST",{},signal);
  }
  rebuild(repoId:string,oid:GitOid,signal?:AbortSignal):Promise<ShadowMapping>{
    return this.request(`${this.prefix(repoId)}/mappings/${encodeURIComponent(oid)}/rebuild`,"POST",{},signal);
  }
}

/** V0.6 authority transitions are operator-only; this client cannot cut over. */
export type AuthorityMode =
  | "GIT_AUTHORITY" | "SHADOW_QUALIFYING" | "CUTOVER_PREPARING"
  | "CUTOVER_FROZEN" | "NATIVE_AUTHORITY_PREVIEW" | "FALLBACK_PREPARING"
  | "FALLBACK_FROZEN" | "READ_ONLY" | "QUARANTINED";
export interface RepositoryAuthority {
  repo_id:string; mode:AuthorityMode; epoch:number;
  checkpoint_git_oid:string|null; checkpoint_native_id:string|null;
  backup_id:string|null;
}
export interface NativeRefSnapshot {
  repo_id:string; ref_name:string; target_changeset_id:string;
  generation:number; authority_epoch:number;
}
export interface NativeRefCasRequest {
  repo_id:string;ref_name:string;expected_target:string;
  expected_generation:number;authority_epoch:number;new_target:string;
  idempotency_key:string;policy_evidence:string;
}
export interface GitMirrorStatus {
  repository_id:string;source_ref:string;native_id:string;
  native_generation:number;mirror_ref:string;git_oid:string;
  mirrored_generation:number;mirror_lag:number;
}
/** The native writer currently accepts verified imported Changesets only. */
export class AuthorityPreviewClient {
  constructor(private readonly endpoint:string,private readonly bearer:string) {}
  private async request<T>(path:string,method:string="GET",body?:unknown,signal?:AbortSignal):Promise<T>{
    const res=await fetch(`${this.endpoint.replace(/\/$/,"")}${path}`,{
      method,headers:{Authorization:`Bearer ${this.bearer}`,...(body===undefined?{}:{"Content-Type":"application/json"})},
      body:body===undefined?undefined:JSON.stringify(body),signal
    });
    if(!res.ok){
      const payload:{error?:{code?:string;message?:string}}=await res.json().catch(()=>({}));
      throw new OpenScionError(payload.error?.code??"HTTP_ERROR",res.status,payload.error?.message??`HTTP ${res.status}`);
    }
    return (await res.json() as {data:T}).data;
  }
  private prefix(repoId:string):string{return `/v1/repos/${encodeURIComponent(repoId)}`;}
  getAuthority(repoId:string,signal?:AbortSignal):Promise<RepositoryAuthority>{
    return this.request(`${this.prefix(repoId)}/authority`,"GET",undefined,signal);
  }
  getNativeRef(repoId:string,refName:string="refs/heads/main",signal?:AbortSignal):Promise<NativeRefSnapshot>{
    return this.request(`${this.prefix(repoId)}/native/refs?ref_name=${encodeURIComponent(refName)}`,"GET",undefined,signal);
  }
  casNativeRef(request:NativeRefCasRequest,signal?:AbortSignal):Promise<NativeRefSnapshot>{
    return this.request(`${this.prefix(request.repo_id)}/native/refs/cas`,"POST",request,signal);
  }
  mirrorStatus(repoId:string,refName:string="refs/heads/main",signal?:AbortSignal):Promise<GitMirrorStatus>{
    return this.request(`${this.prefix(repoId)}/git-mirror/status?ref_name=${encodeURIComponent(refName)}`,"GET",undefined,signal);
  }
  syncMirror(repoId:string,refName:string="refs/heads/main",signal?:AbortSignal):Promise<GitMirrorStatus>{
    return this.request(`${this.prefix(repoId)}/git-mirror/sync`,"POST",{ref_name:refName},signal);
  }
}
