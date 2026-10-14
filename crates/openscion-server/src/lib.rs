//! OpenScion V0.5: Git authority, agent workflows, and read-only shadow-native diagnostics.
use anyhow::Result;
use axum::{body::Body,extract::{Path as AxPath,Query,State},http::{header,HeaderMap,HeaderValue,StatusCode},response::{IntoResponse,Response},routing::{get,post},Json,Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD,Engine as _};
use openscion_git::GitRepository;
use openscion_registry::{Principal,Registry,RepoRecord};
use openscion_workspace::{WorkspaceManager,CreateRequest,CommitRequest};
use openscion_change::{ChangeService,SubmitChange};
use openscion_land::{LandService,PreviewRequest,ReviewRequest,TrustedAttestation};
use openscion_shadow::ShadowService;
use openscion_authority::AuthorityService;
use openscion_native_ref::{NativeRefService,CasRequest};
use openscion_gateway::GatewayService;
use serde::Deserialize;
use serde_json::{json,Value};
use std::{path::PathBuf,sync::Arc};
use tokio::{sync::Semaphore,task};

#[derive(Clone)]
pub struct AppState { pub registry:Registry,pub permits:Arc<Semaphore> }
impl AppState { pub fn new(registry:Registry,max_git_operations:usize)->Self {Self{registry,permits:Arc::new(Semaphore::new(max_git_operations.max(1)))}} }

pub struct ApiError {status:StatusCode,code:&'static str,message:&'static str}
impl ApiError {
 fn new(status:StatusCode,code:&'static str,message:&'static str)->Self{Self{status,code,message}}
 fn auth()->Self{Self::new(StatusCode::UNAUTHORIZED,"UNAUTHENTICATED","A valid bearer token is required")}
 fn denied()->Self{Self::new(StatusCode::FORBIDDEN,"PERMISSION_DENIED","Insufficient permissions")}
 fn not_found()->Self{Self::new(StatusCode::NOT_FOUND,"REPOSITORY_NOT_FOUND","Repository not found")}
 fn bad()->Self{Self::new(StatusCode::BAD_REQUEST,"INVALID_REQUEST","Invalid query parameters")}
 fn backend()->Self{Self::new(StatusCode::UNPROCESSABLE_ENTITY,"GIT_QUERY_FAILED","Git operation failed, missing, or unsupported")}
 fn internal()->Self{Self::new(StatusCode::INTERNAL_SERVER_ERROR,"INTERNAL_ERROR","Internal service failure")}
}
impl IntoResponse for ApiError {
 fn into_response(self)->Response{(self.status,Json(json!({"api_version":"v1","error":{"code":self.code,"message":self.message}}))).into_response()}
}
type ApiResult<T> = std::result::Result<T,ApiError>;
fn principal(state:&AppState,headers:&HeaderMap)->ApiResult<Principal>{
    let v=headers.get(header::AUTHORIZATION).and_then(|h|h.to_str().ok()).ok_or_else(ApiError::auth)?;
    let token=v.strip_prefix("Bearer ").ok_or_else(ApiError::auth)?;
    state.registry.authenticate(token).map_err(|_|ApiError::internal())?.ok_or_else(ApiError::auth)
}
fn authorized(state:&AppState,headers:&HeaderMap,id:&str)->ApiResult<RepoRecord>{
    let who=principal(state,headers)?;
    state.registry.get_authorized(id,&who).map_err(|_|ApiError::internal())?.ok_or_else(ApiError::not_found)
}
async fn execute_git<T,F>(state:&AppState,record:RepoRecord,action:F)->ApiResult<T>
where T:Send+'static,F:FnOnce(GitRepository)->Result<T>+Send+'static {
    let permit=state.permits.clone().acquire_owned().await.map_err(|_|ApiError::internal())?;
    let path=record.path;
    task::spawn_blocking(move ||{
        let _permit=permit;
        let repo=GitRepository::open(&path)?;
        action(repo)
    }).await.map_err(|_|ApiError::internal())?.map_err(|_|ApiError::backend())
}
fn decode_path(raw:Option<String>)->ApiResult<Vec<u8>>{
    let raw=raw.unwrap_or_default();
    if raw.len()>6000{return Err(ApiError::bad());}
    URL_SAFE_NO_PAD.decode(raw).map_err(|_|ApiError::bad())
}
async fn health_live()->Json<Value>{Json(json!({"status":"live"}))}
async fn health_ready(State(state):State<Arc<AppState>>)->ApiResult<Json<Value>>{
    // Lightweight; does not scan all repository object databases.
    state.registry.list(&Principal{id:"admin".into(),role:"admin".into()}).map_err(|_|ApiError::internal())?;
    Ok(Json(json!({"status":"ready"})))
}
async fn list_repos(State(state):State<Arc<AppState>>,headers:HeaderMap)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let rows=state.registry.list(&who).map_err(|_|ApiError::internal())?;
    Ok(Json(json!({"api_version":"v1","data":rows})))
}
#[derive(Deserialize)]
struct RegisterRequest {name:String,local_path:PathBuf}
async fn register_repo(State(state):State<Arc<AppState>>,headers:HeaderMap,Json(input):Json<RegisterRequest>)->ApiResult<(StatusCode,Json<Value>)>{
    let who=principal(&state,&headers)?;
    if who.role!="admin" {return Err(ApiError::denied());}
    let registry=state.registry.clone();let name=input.name;let path=input.local_path;
    let record=task::spawn_blocking(move||registry.register(&name,&path)).await.map_err(|_|ApiError::internal())?.map_err(|_|ApiError::bad())?;
    state.registry.audit(&who.id,"repository.register",&record.id,"ok").map_err(|_|ApiError::internal())?;
    Ok((StatusCode::CREATED,Json(json!({"api_version":"v1","data":record}))))
}
async fn get_repo(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>>{
    let record=authorized(&state,&headers,&id)?;
    let info=execute_git(&state,record.clone(),|g|g.info()).await?;
    Ok(Json(json!({"api_version":"v1","data":{"repository":record,"git":info}})))
}
async fn list_refs(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let refs=execute_git(&state,rec,|g|g.refs()).await?;
    Ok(Json(json!({"api_version":"v1","data":refs})))
}
#[derive(Deserialize)] struct RefQuery {name:String}
async fn resolve_ref(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<RefQuery>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let resolved=execute_git(&state,rec,move|g|g.resolve_ref(&q.name)).await?;
    Ok(Json(json!({"api_version":"v1","data":{"resolved_oid":resolved}})))
}
async fn get_commit(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath((id,oid)):AxPath<(String,String)>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let data=execute_git(&state,rec,move|g|g.read_commit(&oid)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
#[derive(Deserialize)] struct GitPathQuery {path_b64:Option<String>}
async fn get_tree(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath((id,oid)):AxPath<(String,String)>,Query(q):Query<GitPathQuery>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let path=decode_path(q.path_b64)?;
    let data=execute_git(&state,rec,move|g|g.tree_at(&oid,&path)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn get_file(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath((id,oid)):AxPath<(String,String)>,Query(q):Query<GitPathQuery>)->ApiResult<Response>{
    let rec=authorized(&state,&headers,&id)?; let path=decode_path(q.path_b64)?;
    let file=execute_git(&state,rec,move|g|g.file_at(&oid,&path,16*1024*1024)).await?;
    let mut response=Response::new(Body::from(file.content));
    response.headers_mut().insert(header::CONTENT_TYPE,HeaderValue::from_static("application/octet-stream"));
    if let Ok(v)=HeaderValue::from_str(&file.blob_oid){response.headers_mut().insert("x-git-object-id",v);}
    Ok(response)
}
#[derive(Deserialize)] struct Pair {base:String,target:String}
async fn changed_paths(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<Pair>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let data=execute_git(&state,rec,move|g|g.changed_paths(&q.base,&q.target)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn diff_patch(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<Pair>)->ApiResult<Response>{
    let rec=authorized(&state,&headers,&id)?;
    let patch=execute_git(&state,rec,move|g|g.diff_patch(&q.base,&q.target)).await?;
    let mut response=Response::new(Body::from(patch));
    response.headers_mut().insert(header::CONTENT_TYPE,HeaderValue::from_static("application/octet-stream"));
    Ok(response)
}
#[derive(Deserialize)] struct MergePair {left:String,right:String}
async fn merge_bases(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<MergePair>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let data=execute_git(&state,rec,move|g|g.merge_bases(&q.left,&q.right)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
#[derive(Deserialize)] struct HistoryQuery {start:String,skip:Option<usize>,limit:Option<usize>,first_parent:Option<bool>}
async fn history(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<HistoryQuery>)->ApiResult<Json<Value>>{
    let rec=authorized(&state,&headers,&id)?;
    let data=execute_git(&state,rec,move|g|g.history(&q.start,q.skip.unwrap_or(0),q.limit.unwrap_or(50),q.first_parent.unwrap_or(false))).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}


fn ws_error(e:anyhow::Error)->ApiError {
    let m=e.to_string();
    if m.contains("PERMISSION_DENIED") {return ApiError::denied();}
    if m.contains("WORKSPACE_NOT_FOUND") {return ApiError::new(StatusCode::NOT_FOUND,"WORKSPACE_NOT_FOUND","Workspace unavailable");}
    if m.contains("WORKSPACE_DIRTY") {return ApiError::new(StatusCode::CONFLICT,"WORKSPACE_DIRTY","Uncommitted changes prevent disposal");}
    if m.contains("WORKSPACE_HEAD_CHANGED") {return ApiError::new(StatusCode::CONFLICT,"WORKSPACE_HEAD_CHANGED","Workspace HEAD changed");}
    if m.contains("WORKSPACE_EXPIRED") {return ApiError::new(StatusCode::CONFLICT,"WORKSPACE_EXPIRED","Workspace lease expired");}
    if m.contains("WORKSPACE_IDEMPOTENCY_CONFLICT") {return ApiError::new(StatusCode::CONFLICT,"WORKSPACE_IDEMPOTENCY_CONFLICT","Idempotency key already used");}
    if m.contains("WORKSPACE_INVALID_STATE") || m.contains("WORKSPACE_BUSY") || m.contains("WORKSPACE_STATE_CONFLICT") {return ApiError::new(StatusCode::CONFLICT,"WORKSPACE_INVALID_STATE","Workspace is unavailable for operation");}
    if m.contains("WORKSPACE_LIMIT_EXCEEDED") {return ApiError::new(StatusCode::TOO_MANY_REQUESTS,"WORKSPACE_LIMIT_EXCEEDED","Workspace quota exceeded");}
    if m.contains("INVALID_") {return ApiError::bad();}
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"WORKSPACE_OPERATION_FAILED","Workspace operation failed; inspect operation and recovery state")
}
async fn workspace_exec<T,F>(state:&AppState,action:F)->ApiResult<T>
where T:Send+'static,F:FnOnce(WorkspaceManager)->Result<T>+Send+'static {
    let permit=state.permits.clone().acquire_owned().await.map_err(|_|ApiError::internal())?;
    let registry=state.registry.clone();
    task::spawn_blocking(move||{let _permit=permit;action(WorkspaceManager::new(registry))})
        .await.map_err(|_|ApiError::internal())?.map_err(ws_error)
}
async fn ws_create(State(state):State<Arc<AppState>>,headers:HeaderMap,Json(req):Json<CreateRequest>)->ApiResult<(StatusCode,Json<Value>)>{
    let who=principal(&state,&headers)?;
    let row=workspace_exec(&state,move|mgr|mgr.create(&who,req)).await?;
    Ok((StatusCode::CREATED,Json(json!({"api_version":"v1","data":row}))))
}
async fn ws_list(State(state):State<Arc<AppState>>,headers:HeaderMap)->ApiResult<Json<Value>> {
    let who=principal(&state,&headers)?;
    let rows=workspace_exec(&state,move|mgr|mgr.list(&who)).await?;
    Ok(Json(json!({"api_version":"v1","data":rows})))
}
async fn ws_get(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let row=workspace_exec(&state,move|mgr|mgr.get(&who,&id)).await?;
    Ok(Json(json!({"api_version":"v1","data":row})))
}
async fn ws_status(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let result=workspace_exec(&state,move|mgr|mgr.status(&who,&id)).await?;
    Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn ws_diff(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Response>{
    let who=principal(&state,&headers)?;
    let bytes=workspace_exec(&state,move|mgr|mgr.diff(&who,&id)).await?;
    let mut response=Response::new(Body::from(bytes));
    response.headers_mut().insert(header::CONTENT_TYPE,HeaderValue::from_static("application/octet-stream"));
    Ok(response)
}
async fn ws_commit(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(req):Json<CommitRequest>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let row=workspace_exec(&state,move|mgr|mgr.commit(&who,&id,req)).await?;
    Ok(Json(json!({"api_version":"v1","data":row})))
}
#[derive(Deserialize)] struct LeaseRequest {lease_seconds:u64}
async fn ws_renew(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(req):Json<LeaseRequest>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let row=workspace_exec(&state,move|mgr|mgr.renew(&who,&id,req.lease_seconds)).await?;
    Ok(Json(json!({"api_version":"v1","data":row})))
}
async fn ws_dispose(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let row=workspace_exec(&state,move|mgr|mgr.dispose(&who,&id)).await?;
    Ok(Json(json!({"api_version":"v1","data":row})))
}


fn change_error(e:anyhow::Error)->ApiError {
 let s=e.to_string();
 if s.contains("PERMISSION_DENIED") || s.contains("INDEPENDENT_REVIEW_REQUIRED") || s.contains("RUNNER_UNTRUSTED") {return ApiError::denied();}
 if s.contains("_NOT_FOUND") {return ApiError::new(StatusCode::NOT_FOUND,"CHANGE_NOT_FOUND","Change or operation unavailable");}
 if s.contains("INVALID_") {return ApiError::bad();}
 if s.contains("CONFLICT") || s.contains("REQUIRED") || s.contains("_MISMATCH") || s.contains("_CHANGED") || s.contains("_STATE") || s.contains("_EFFECT") {
    return ApiError::new(StatusCode::CONFLICT,"CHANGE_PRECONDITION_FAILED","Integration precondition failed; inspect operation state");
 }
 ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"CHANGE_OPERATION_FAILED","Change processing failed; inspect operation state")
}
async fn change_exec<T,F>(state:&AppState,action:F)->ApiResult<T>
where T:Send+'static,F:FnOnce(Registry)->Result<T>+Send+'static {
 let permit=state.permits.clone().acquire_owned().await.map_err(|_|ApiError::internal())?;
 let reg=state.registry.clone();
 task::spawn_blocking(move||{let _permit=permit;action(reg)})
  .await.map_err(|_|ApiError::internal())?.map_err(change_error)
}
async fn changes_list(State(state):State<Arc<AppState>>,headers:HeaderMap)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let rows=change_exec(&state,move|r|ChangeService::new(r).list(&who)).await?;
 Ok(Json(json!({"api_version":"v1","data":rows})))
}
async fn changes_submit(State(state):State<Arc<AppState>>,headers:HeaderMap,Json(req):Json<SubmitChange>)->ApiResult<(StatusCode,Json<Value>)> {
 let who=principal(&state,&headers)?;
 let row=change_exec(&state,move|r|ChangeService::new(r).submit(&who,req)).await?;
 Ok((StatusCode::CREATED,Json(json!({"api_version":"v1","data":row}))))
}
async fn changes_get(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let row=change_exec(&state,move|r|ChangeService::new(r).get(&who,&id)).await?;
 Ok(Json(json!({"api_version":"v1","data":row})))
}
#[derive(Deserialize)] struct PreviewBody {idempotency_key:String}
async fn changes_preview(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(req):Json<PreviewBody>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).start(&who,PreviewRequest{change_id:id,idempotency_key:req.idempotency_key})).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_get(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).get(&who,&id)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_validate(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).validate(&who,&id)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_attest(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(proof):Json<TrustedAttestation>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).attest(&who,&id,proof)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_review(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(req):Json<ReviewRequest>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).review(&who,&id,&req.decision)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_commit(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).land(&who,&id)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_reconcile(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).reconcile(&who,&id)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn land_repreview(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=change_exec(&state,move|r|LandService::new(r).repreview(&who,&id)).await?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}


// V0.5 shadow-native diagnostic and administrative APIs.
// Native data is never promoted into Git authority by these handlers.
fn shadow_error(e:anyhow::Error)->ApiError {
    let msg=e.to_string();
    if msg.contains("PERMISSION_DENIED") {return ApiError::denied();}
    if msg.contains("NOT_FOUND") {return ApiError::new(StatusCode::NOT_FOUND,"SHADOW_NOT_FOUND","Shadow resource unavailable");}
    if msg.contains("INVALID") {return ApiError::bad();}
    if msg.contains("MISMATCH") {return ApiError::new(StatusCode::CONFLICT,"SHADOW_PARITY_MISMATCH","Native/Git parity verification failed");}
    if msg.contains("INELIGIBLE") || msg.contains("BUDGET_EXCEEDED") {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"SHADOW_INELIGIBLE","Commit not supported by shadow importer");
    }
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"SHADOW_OPERATION_FAILED","Shadow operation failed; see authorized job diagnostics")
}
async fn execute_shadow<T,F>(state:&AppState,action:F)->ApiResult<T>
where T:Send+'static,F:FnOnce(Registry)->Result<T>+Send+'static {
    let permit=state.permits.clone().acquire_owned().await.map_err(|_|ApiError::internal())?;
    let registry=state.registry.clone();
    task::spawn_blocking(move||{let _permit=permit;action(registry)})
        .await.map_err(|_|ApiError::internal())?.map_err(shadow_error)
}
#[derive(Deserialize)]struct ShadowEnableRequest{tracked_ref:String}
#[derive(Deserialize)]struct ShadowOidRequest{git_oid:String}
#[derive(Deserialize)]struct ShadowWorkRequest{max_jobs:usize}
#[derive(Deserialize)]struct ShadowVerifyQuery{limit:Option<usize>}
async fn shadow_status(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).status(&who,&repo)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_enable(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>,Json(req):Json<ShadowEnableRequest>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).enable(&who,&repo,&req.tracked_ref)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_enqueue(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>,Json(req):Json<ShadowOidRequest>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).enqueue(&who,&repo,&req.git_oid)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_observe(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).observe(&who,&repo)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_work(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>,Json(req):Json<ShadowWorkRequest>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).work(&who,&repo,req.max_jobs)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_jobs(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).jobs(&who,&repo)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_mapping(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath((repo,oid)):AxPath<(String,String)>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).mapping(&who,&repo,&oid)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_retry(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath((repo,job)):AxPath<(String,String)>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).retry_failed(&who,&repo,&job)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_rebuild(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath((repo,oid)):AxPath<(String,String)>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).rebuild_derived(&who,&repo,&oid)).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}
async fn shadow_verify(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(repo):AxPath<String>,Query(query):Query<ShadowVerifyQuery>)->ApiResult<Json<Value>>{
    let who=principal(&state,&headers)?;
    let data=execute_shadow(&state,move|reg|ShadowService::new(reg).verify_sample(&who,&repo,query.limit.unwrap_or(10))).await?;
    Ok(Json(json!({"api_version":"v1","data":data})))
}


fn v06_error(err:anyhow::Error)->ApiError {
 let msg=err.to_string();
 match msg.as_str() {
  "PERMISSION_DENIED" => ApiError::denied(),
  "AUTHORITY_FENCED"|"AUTHORITY_MODE_CONFLICT" => ApiError::new(StatusCode::CONFLICT,"AUTHORITY_FENCED","Repository authority epoch or mode no longer permits this operation"),
  "NATIVE_REF_CAS_CONFLICT" => ApiError::new(StatusCode::CONFLICT,"NATIVE_REF_CAS_CONFLICT","Native reference changed since the expected generation"),
  "NATIVE_NON_FAST_FORWARD" => ApiError::new(StatusCode::CONFLICT,"NATIVE_NON_FAST_FORWARD","Native Ref movement is not a fast-forward"),
  "NATIVE_REF_NO_EFFECT" => ApiError::new(StatusCode::CONFLICT,"NATIVE_REF_NO_EFFECT","Native reference already identifies this version"),
  "NATIVE_AUTHORITY_NOT_ENABLED" => ApiError::new(StatusCode::CONFLICT,"NATIVE_AUTHORITY_NOT_ENABLED","Native authority preview is not enabled"),
  "GIT_MIRROR_LAGGING" => ApiError::new(StatusCode::CONFLICT,"GIT_MIRROR_LAGGING","Git mirror checkpoint is not available"),
  "GIT_EXPORT_INELIGIBLE" => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"GIT_EXPORT_INELIGIBLE","The native Changeset has no verified Git mapping"),
  "NATIVE_OBJECT_CLOSURE_INCOMPLETE"|"NATIVE_TARGET_NOT_VERIFIED" => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"NATIVE_OBJECT_CLOSURE_INCOMPLETE","Required verified immutable objects are unavailable"),
  "IDEMPOTENCY_CONFLICT" => ApiError::new(StatusCode::CONFLICT,"IDEMPOTENCY_CONFLICT","Request key was reused for another operation"),
  _ => ApiError::backend(),
 }
}

// V0.6 read-only authority visibility and preview-gated native Ref APIs.
// Operator promotion, backup, and fallback deliberately remain local-CLI-only.
async fn authority_get(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let data=AuthorityService::new(state.registry.clone()).read(&who,&id).map_err(|_|ApiError::not_found())?;
 Ok(Json(json!({"api_version":"v1","data":data})))
}
#[derive(Deserialize)]struct NativeRefQuery {ref_name:String}
async fn native_ref_get(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<NativeRefQuery>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=NativeRefService::new(state.registry.clone()).read(&who,&id,&q.ref_name).map_err(v06_error)?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn native_ref_cas(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(req):Json<CasRequest>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 if who.role!="admin" {return Err(ApiError::denied());}
 if id!=req.repo_id{return Err(ApiError::bad());}
 let permit=state.permits.clone().acquire_owned().await.map_err(|_|ApiError::internal())?;
 let reg=state.registry.clone();
 let result=task::spawn_blocking(move||{let _permit=permit;NativeRefService::new(reg).cas(&who,req)})
   .await.map_err(|_|ApiError::internal())?.map_err(v06_error)?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn mirror_status(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Query(q):Query<NativeRefQuery>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 let result=GatewayService::new(state.registry.clone()).status(&who,&id,&q.ref_name).map_err(v06_error)?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}
async fn mirror_sync(State(state):State<Arc<AppState>>,headers:HeaderMap,AxPath(id):AxPath<String>,Json(q):Json<NativeRefQuery>)->ApiResult<Json<Value>> {
 let who=principal(&state,&headers)?;
 if who.role!="admin" {return Err(ApiError::denied());}
 let permit=state.permits.clone().acquire_owned().await.map_err(|_|ApiError::internal())?;
 let reg=state.registry.clone();
 let result=task::spawn_blocking(move||{let _permit=permit;GatewayService::new(reg).sync_verified(&who,&id,&q.ref_name)})
   .await.map_err(|_|ApiError::internal())?.map_err(v06_error)?;
 Ok(Json(json!({"api_version":"v1","data":result})))
}

pub fn router(state:AppState)->Router {
    Router::new()
        .route("/health/live",get(health_live))
        .route("/health/ready",get(health_ready))
        .route("/v1/repos",get(list_repos).post(register_repo))
        .route("/v1/repos/:id",get(get_repo))
        .route("/v1/repos/:id/refs",get(list_refs))
        .route("/v1/repos/:id/refs/resolve",get(resolve_ref))
        .route("/v1/repos/:id/commits/:oid",get(get_commit))
        .route("/v1/repos/:id/trees/:oid",get(get_tree))
        .route("/v1/repos/:id/files/:oid",get(get_file))
        .route("/v1/repos/:id/changed-paths",get(changed_paths))
        .route("/v1/repos/:id/diff",get(diff_patch))
        .route("/v1/repos/:id/merge-bases",get(merge_bases))
        .route("/v1/repos/:id/history",get(history))
        .route("/v1/workspaces",get(ws_list).post(ws_create))
        .route("/v1/workspaces/:id",get(ws_get).delete(ws_dispose))
        .route("/v1/workspaces/:id/status",get(ws_status))
        .route("/v1/workspaces/:id/diff",get(ws_diff))
        .route("/v1/workspaces/:id/commits",post(ws_commit))
        .route("/v1/workspaces/:id/lease",post(ws_renew))
        .route("/v1/change-requests",get(changes_list).post(changes_submit))
        .route("/v1/change-requests/:id",get(changes_get))
        .route("/v1/change-requests/:id/preview",post(changes_preview))
        .route("/v1/land-operations/:id",get(land_get))
        .route("/v1/land-operations/:id/validate",post(land_validate))
        .route("/v1/land-operations/:id/attest",post(land_attest))
        .route("/v1/land-operations/:id/reviews",post(land_review))
        .route("/v1/land-operations/:id/land",post(land_commit))
        .route("/v1/land-operations/:id/reconcile",post(land_reconcile))
        .route("/v1/land-operations/:id/repreview",post(land_repreview))
        .route("/v1/repos/:id/authority",get(authority_get))
        .route("/v1/repos/:id/native/refs",get(native_ref_get))
        .route("/v1/repos/:id/native/refs/cas",post(native_ref_cas))
        .route("/v1/repos/:id/git-mirror/status",get(mirror_status))
        .route("/v1/repos/:id/git-mirror/sync",post(mirror_sync))
        .route("/v1/repos/:id/shadow",get(shadow_status))
        .route("/v1/repos/:id/shadow/enable",post(shadow_enable))
        .route("/v1/repos/:id/shadow/enqueue",post(shadow_enqueue))
        .route("/v1/repos/:id/shadow/observe",post(shadow_observe))
        .route("/v1/repos/:id/shadow/work",post(shadow_work))
        .route("/v1/repos/:id/shadow/jobs",get(shadow_jobs))
        .route("/v1/repos/:id/shadow/mappings/:oid",get(shadow_mapping))
        .route("/v1/repos/:id/shadow/verify",get(shadow_verify))
        .route("/v1/repos/:id/shadow/jobs/:job/retry",post(shadow_retry))
        .route("/v1/repos/:id/shadow/mappings/:oid/rebuild",post(shadow_rebuild))
        .with_state(Arc::new(state))
}
