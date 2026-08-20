//! Local intelligence commands.
//!
//! Every one degrades rather than failing the application. A node with no model
//! answers "unavailable" and carries on; nothing here can prevent an incident
//! being created or replicated.
//!
//! # What is deliberately absent
//!
//! There is no command that lets the frontend — or, through it, a model — reach
//! identity, trust, or synchronisation. The AI surface is: read incidents,
//! produce derived intelligence, answer questions from local records. That
//! restriction is enforced by the service's field list, not by these
//! signatures; see `crate::ai`.

use super::AppState;
use crate::ai::knowledge_pack::InstallReport;
use crate::ai::{
    GroundedAnswer, IncidentIndexState, IndexReport, IntelligenceStatus, KnowledgeBaseSummary,
};
use crate::domain::IncidentAnalysis;
use crate::error::CoreResult;
use crate::storage::intelligence::KnowledgeDocument;
use tauri::State;

/// Model, readiness, and index sizes for the Intelligence panel.
#[tauri::command]
pub fn get_intelligence_status(state: State<'_, AppState>) -> IntelligenceStatus {
    state.runtime.intelligence_status()
}

/// Analyses one incident with the local model.
///
/// Slow — seconds on CPU — so the UI calls it explicitly rather than on every
/// incident. Analysis is never automatic: it must not sit on the path of
/// incident capture.
#[tauri::command]
pub fn analyse_incident(
    state: State<'_, AppState>,
    incident_id: String,
) -> CoreResult<IncidentAnalysis> {
    state.runtime.analyse_incident(&incident_id)
}

/// The stored analysis for an incident, if one exists.
///
/// Returns `None` rather than an error when intelligence is unavailable, so the
/// incident view renders identically on a node with no model.
#[tauri::command]
pub fn get_incident_analysis(
    state: State<'_, AppState>,
    incident_id: String,
) -> CoreResult<Option<IncidentAnalysis>> {
    state.runtime.incident_analysis(&incident_id)
}

/// Answers a question from this node's own records.
#[tauri::command]
pub fn ask_securemesh(
    state: State<'_, AppState>,
    question: String,
    top_k: Option<usize>,
) -> CoreResult<GroundedAnswer> {
    state.runtime.ask_intelligence(&question, top_k)
}

/// Embeds anything not yet indexed.
///
/// Indexing is automatic — a local write and an applied replication each
/// trigger a pass — so this exists for a manual retry, not as the mechanism.
#[tauri::command]
pub fn index_intelligence(state: State<'_, AppState>) -> CoreResult<IndexReport> {
    state.runtime.index_intelligence()
}

/// Where each incident stands in the local vector index.
///
/// Reported separately from `sync_status` because replication and indexing are
/// independent: an incident can be shared with every peer and still not be
/// searchable here, and presenting one as the other would mislead an operator.
#[tauri::command]
pub fn get_incident_index_states(
    state: State<'_, AppState>,
) -> CoreResult<Vec<IncidentIndexState>> {
    state.runtime.incident_index_states()
}

/// Documents in the local knowledge base.
#[tauri::command]
pub fn get_knowledge_documents(state: State<'_, AppState>) -> CoreResult<Vec<KnowledgeDocument>> {
    state.runtime.knowledge_documents()
}

/// Installs the operational knowledge pack that ships inside this binary.
///
/// Explicitly operator-triggered. Nothing is provisioned at startup and nothing
/// is fetched: the documents are compiled in, so this command reads no file and
/// opens no connection.
///
/// Safe to call repeatedly — each document is keyed by a content hash, so a
/// second call reports everything as already present and writes nothing.
#[tauri::command]
pub fn install_operational_knowledge(state: State<'_, AppState>) -> CoreResult<InstallReport> {
    state.runtime.install_operational_knowledge()
}

/// Counts of what local knowledge this node holds, for the Knowledge Base panel.
///
/// Read-only, and valid on a node with no model — it reports an empty knowledge
/// base rather than refusing.
#[tauri::command]
pub fn get_knowledge_summary(state: State<'_, AppState>) -> CoreResult<KnowledgeBaseSummary> {
    state.runtime.knowledge_summary()
}
