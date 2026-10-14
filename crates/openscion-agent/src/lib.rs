//! Framework-neutral agent session and task identifiers.
//! This crate does not execute model-generated code or issue Git mutations.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub session_id: Uuid,
    pub principal_id: String,
    pub task_id: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_name: Option<String>,
    pub state: SessionState,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="SCREAMING_SNAKE_CASE")]
pub enum SessionState { Created, Bound, Active, Paused, Completed, Failed, Cancelled }
impl AgentSession {
    pub fn new(principal_id: String, task_id: Option<String>, agent_name: Option<String>) -> Self {
        Self { session_id: Uuid::new_v4(), principal_id, task_id, workspace_id: None, agent_name, state: SessionState::Created }
    }
}
