use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::Utc;
use gloo_storage::Storage;
use js_sys::{Function, Reflect};
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use sequoia_shared::{
    ClaimDocumentBase, ClaimDocumentV1, ClaimMacro, ClaimOwner, ClaimTerritoryStateOverride,
    ClaimValidationError, ClaimViewState, ClaimsBootstrapGeometry, ClaimsTerritoryGeometry,
    GuildRef, LiveState, Resources, Territory, TerritoryMap, compact_claim_overrides,
    compute_claim_metrics, validate_claim_document,
};

use crate::app::{
    AbbreviateNames, BoldConnections, ConnectionOpacityScale, ConnectionThicknessScale,
    ConnectionZoomFadeEnd, ConnectionZoomFadeStart, CurrentMode, DetailReturnGuild, FillAlphaBoost,
    HeatEntriesByTerritory, HeatMaxTakeCount, HeatModeEnabled, HeatWindowLabel,
    HistoryBufferModeActive, HistoryBufferSizeMax, HistoryBufferedUpdates, HistoryFetchNonce,
    HistoryTimestamp, Hovered, IsMobile, LabelScaleDynamic, LabelScaleIcons, LabelScaleMaster,
    LabelScaleStatic, LabelScaleStaticName, LastLiveSeq, LiveResyncInFlight, MapMode, NameColor,
    NameColorSetting, NeedsLiveResync, PeekTerritory, ReadableFont, ResourceHighlight, Selected,
    ShowCompoundMapTime, ShowCountdown, ShowGranularMapTime, ShowMinimap, ShowNames, ShowSettings,
    ShowTerritoryOrnaments, SidebarOpen, SidebarTransient, SseSeqGapDetectedCount,
    SuppressCooldownVisuals, TagColorSetting, ThickCooldownBorders, canvas_dimensions,
};
use crate::canvas::{ClaimCanvasController, ClaimTool, MapCanvas};
use crate::history;
use crate::sse::{self, ConnectionStatus};
use crate::territory::{ClientTerritory, ClientTerritoryMap};
use crate::tiles::{self, LoadedTile};
use crate::viewport::Viewport;

const DRAFT_STORAGE_KEY: &str = "sequoia_claim_draft_v1";
const PRESET_STORAGE_KEY: &str = "sequoia_claim_presets_v1";
const MACRO_LIBRARY_STORAGE_KEY: &str = "sequoia_claim_macros_v1";
const IMPORT_HANDOFF_STORAGE_KEY: &str = "sequoia_claim_import_handoff_v1";
const LIVE_BOOTSTRAP_STORAGE_KEY: &str = "sequoia_claim_live_bootstrap_v1";
const GEOMETRY_BOOTSTRAP_STORAGE_KEY: &str = "sequoia_claim_geometry_bootstrap_v1";
const SHARE_FRAGMENT_PREFIX: &str = "#c=";
#[cfg(test)]
const MAX_SHARE_FRAGMENT_BYTES: usize = 120_000;
const LIVE_BOOTSTRAP_MAX_AGE_MS: f64 = 120_000.0;
const GEOMETRY_BOOTSTRAP_MAX_AGE_MS: f64 = 3_600_000.0;
const BOOTSTRAP_STORAGE_VERSION: u8 = 1;
const LIVE_SYNC_PENDING_MESSAGE: &str = "Live ownership is still syncing. The board is usable now and will reconcile in the background.";

const NEUTRAL_GUILD_UUID: &str = "__neutral__";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaimTab {
    Territory,
    Summary,
    Compare,
    Macros,
    Share,
}

impl ClaimTab {
    fn label(self) -> &'static str {
        match self {
            ClaimTab::Territory => "Territory",
            ClaimTab::Summary => "Summary",
            ClaimTab::Compare => "Compare",
            ClaimTab::Macros => "Macros",
            ClaimTab::Share => "Share",
        }
    }
}

#[derive(Clone)]
struct ClaimUndoState {
    document: ClaimDocumentV1,
    follow_live: bool,
    selection: Vec<String>,
    active_owner: ClaimOwner,
}

#[derive(Clone)]
struct ClaimWorkingSession {
    document: ClaimDocumentV1,
    follow_live: bool,
    dirty: bool,
    selection: Vec<String>,
    source_snapshot_id: Option<String>,
    source_snapshot_url: Option<String>,
    undo_stack: Vec<ClaimUndoState>,
    redo_stack: Vec<ClaimUndoState>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredClaimDraft {
    document: ClaimDocumentV1,
    follow_live: bool,
    #[serde(default = "stored_claim_draft_dirty_default")]
    dirty: bool,
    selection: Vec<String>,
    source_snapshot_url: Option<String>,
    #[serde(default)]
    source_snapshot_url: Option<String>,
    active_owner: ClaimOwner,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredClaimPreset {
    id: String,
    name: String,
    document: ClaimDocumentV1,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StartupImportHandoff {
    document: ClaimDocumentV1,
    #[serde(default)]
    follow_live: bool,
    #[serde(default)]
    selection: Vec<String>,
    #[serde(default)]
    source_snapshot_id: Option<String>,
    #[serde(default)]
    source_snapshot_url: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StagedLiveBootstrap {
    #[serde(default = "bootstrap_storage_version")]
    version: u8,
    cached_at_ms: f64,
    state: LiveState,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StagedGeometryBootstrap {
    #[serde(default = "bootstrap_storage_version")]
    version: u8,
    cached_at_ms: f64,
    geometry: ClaimsBootstrapGeometry,
}

#[derive(Clone)]
struct ClaimsEditorInit {
    geometry: ClaimsBootstrapGeometry,
    live_state: Option<LiveState>,
    document: ClaimDocumentV1,
    follow_live: bool,
    dirty: bool,
    selection: Vec<String>,
    source_snapshot_id: Option<String>,
    source_snapshot_url: Option<String>,
    active_owner: ClaimOwner,
}

#[derive(Clone)]
enum ClaimsBootPayload {
    Blank(ClaimsEditorInit),
    Live(ClaimsEditorInit),
    Draft(ClaimsEditorInit),
    Import(ClaimsEditorInit),
    Saved(ClaimsEditorInit),
}

impl ClaimsBootPayload {
    fn into_editor_init(self) -> ClaimsEditorInit {
        match self {
            ClaimsBootPayload::Blank(init)
            | ClaimsBootPayload::Live(init)
            | ClaimsBootPayload::Draft(init)
            | ClaimsBootPayload::Import(init)
            | ClaimsBootPayload::Saved(init) => init,
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedClaimResponse {
    id: String,
    created_at: String,
    url: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedClaimDocumentResponse {
    id: String,
    created_at: String,
    title: Option<String>,
    document: ClaimDocumentV1,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct GuildCatalogResponse {
    guilds: Vec<GuildCatalogEntry>,
    cached_at: String,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct GuildCatalogEntry {
    uuid: String,
    name: String,
    prefix: String,
    color: Option<(u8, u8, u8)>,
}

#[derive(Clone)]
enum ClaimsRoute {
    Root,
    NewBlank,
    NewLive,
    Draft,
    Import,
    Saved(String),
}

const fn bootstrap_storage_version() -> u8 {
    BOOTSTRAP_STORAGE_VERSION
}

const fn stored_claim_draft_dirty_default() -> bool {
    true
}

fn parse_claims_route(path: &str) -> ClaimsRoute {
    match path.trim_end_matches('/') {
        "/claims/new/blank" => return ClaimsRoute::NewBlank,
        "/claims/new/live" => return ClaimsRoute::NewLive,
        "/claims/new/draft" => return ClaimsRoute::Draft,
        "/claims/new/import" => return ClaimsRoute::Import,
        _ => {}
    }
    if let Some(saved_id) = path.strip_prefix("/claims/s/") {
        let saved_id = saved_id.trim_matches('/');
        if !saved_id.is_empty() {
            return ClaimsRoute::Saved(saved_id.to_string());
        }
    }
    ClaimsRoute::Root
}

fn current_hash_document() -> Result<Option<ClaimDocumentV1>, String> {
    let hash = web_sys::window()
        .and_then(|window| window.location().hash().ok())
        .unwrap_or_default();
    if !hash.starts_with(SHARE_FRAGMENT_PREFIX) {
        return Ok(None);
    }
    let encoded = &hash[SHARE_FRAGMENT_PREFIX.len()..];
    decode_claim_fragment(encoded).map(Some)
}

fn territory_map_from_client(map: &ClientTerritoryMap) -> TerritoryMap {
    map.iter()
        .map(|(name, territory)| (name.clone(), territory.territory.clone()))
        .collect()
}

fn neutral_owner() -> ClaimOwner {
    ClaimOwner::Neutral
}

fn neutral_guild_ref() -> GuildRef {
    GuildRef {
        uuid: NEUTRAL_GUILD_UUID.to_string(),
        name: "Neutral".to_string(),
        prefix: String::new(),
        color: Some((132, 144, 166)),
    }
}

fn owner_from_live_guild(guild: &GuildRef) -> ClaimOwner {
    ClaimOwner::from_guild(guild.clone())
}

fn owner_to_guild(owner: &ClaimOwner) -> GuildRef {
    match owner {
        ClaimOwner::Neutral => neutral_guild_ref(),
        ClaimOwner::Guild { guild } => guild.clone(),
    }
}

fn owner_identity(owner: &ClaimOwner) -> Option<String> {
    match owner {
        ClaimOwner::Neutral => None,
        ClaimOwner::Guild { guild } => {
            if !guild.uuid.trim().is_empty() {
                Some(guild.uuid.clone())
            } else {
                Some(guild.name.to_ascii_lowercase())
            }
        }
    }
}

fn current_live_owner_map(live_territories: &ClientTerritoryMap) -> HashMap<String, ClaimOwner> {
    live_territories
        .iter()
        .map(|(name, territory)| {
            (
                name.clone(),
                owner_from_live_guild(&territory.territory.guild),
            )
        })
        .collect()
}

fn document_base_owner(document: &ClaimDocumentV1, territory: &str) -> ClaimOwner {
    match &document.base {
        ClaimDocumentBase::Blank => ClaimOwner::Neutral,
        ClaimDocumentBase::FrozenLiveSnapshot { owners, .. } => owners
            .get(territory)
            .cloned()
            .unwrap_or(ClaimOwner::Neutral),
    }
}

fn effective_owner_for_session(
    session: &ClaimWorkingSession,
    live_owners: &HashMap<String, ClaimOwner>,
    territory: &str,
) -> ClaimOwner {
    session
        .document
        .overrides
        .get(territory)
        .cloned()
        .unwrap_or_else(|| {
            if session.follow_live {
                live_owners
                    .get(territory)
                    .cloned()
                    .unwrap_or(ClaimOwner::Neutral)
            } else {
                document_base_owner(&session.document, territory)
            }
        })
}

fn snapshot_session(session: &ClaimWorkingSession, active_owner: &ClaimOwner) -> ClaimUndoState {
    ClaimUndoState {
        document: session.document.clone(),
        follow_live: session.follow_live,
        selection: session.selection.clone(),
        active_owner: active_owner.clone(),
    }
}

fn push_undo_state(session: &mut ClaimWorkingSession, active_owner: &ClaimOwner) {
    session
        .undo_stack
        .push(snapshot_session(session, active_owner));
    if session.undo_stack.len() > 64 {
        session.undo_stack.remove(0);
    }
    session.redo_stack.clear();
    session.dirty = true;
}

fn set_effective_owner(
    session: &mut ClaimWorkingSession,
    territory: &str,
    next_owner: ClaimOwner,
    live_owners: &HashMap<String, ClaimOwner>,
) -> bool {
    let current = effective_owner_for_session(session, live_owners, territory);
    if current == next_owner {
        return false;
    }

    let base_owner = if session.follow_live {
        live_owners
            .get(territory)
            .cloned()
            .unwrap_or(ClaimOwner::Neutral)
    } else {
        document_base_owner(&session.document, territory)
    };

    if next_owner == base_owner {
        session.document.overrides.remove(territory);
    } else {
        session
            .document
            .overrides
            .insert(territory.to_string(), next_owner);
    }
    session.dirty = true;
    true
}

fn base_resources_for_session(live_territories: &ClientTerritoryMap, territory: &str) -> Resources {
    live_territories
        .get(territory)
        .map(|territory| territory.territory.resources.clone())
        .unwrap_or_default()
}

fn effective_resources_for_session(
    session: &ClaimWorkingSession,
    live_territories: &ClientTerritoryMap,
    territory: &str,
) -> Resources {
    session
        .document
        .territory_state_overrides
        .get(territory)
        .and_then(|entry| entry.resources.clone())
        .unwrap_or_else(|| base_resources_for_session(live_territories, territory))
}

fn set_effective_resources(
    session: &mut ClaimWorkingSession,
    territory: &str,
    next_resources: Resources,
    live_territories: &ClientTerritoryMap,
) -> bool {
    let current = effective_resources_for_session(session, live_territories, territory);
    if current == next_resources {
        return false;
    }

    let base_resources = base_resources_for_session(live_territories, territory);
    if next_resources == base_resources {
        session.document.territory_state_overrides.remove(territory);
    } else {
        let entry = session
            .document
            .territory_state_overrides
            .entry(territory.to_string())
            .or_insert_with(ClaimTerritoryStateOverride::default);
        entry.resources = Some(next_resources);
        if entry.is_empty() {
            session.document.territory_state_overrides.remove(territory);
        }
    }
    session.dirty = true;
    true
}

fn selection_focus(selection: &[String], preferred: Option<&str>) -> Option<String> {
    preferred
        .filter(|territory| selection.iter().any(|name| name == territory))
        .map(ToOwned::to_owned)
        .or_else(|| selection.last().cloned())
}

fn apply_selected_owner_override(
    selected: RwSignal<Option<String>>,
    live_territories: RwSignal<ClientTerritoryMap>,
    session: RwSignal<Option<ClaimWorkingSession>>,
    active_owner: RwSignal<ClaimOwner>,
    next_owner: ClaimOwner,
) {
    let Some(territory_name) = selected.get_untracked() else {
        return;
    };
    let live_owners = current_live_owner_map(&live_territories.get_untracked());
    session.update(|session_state| {
        let Some(session_state) = session_state.as_mut() else {
            return;
        };
        push_undo_state(session_state, &active_owner.get_untracked());
        if !set_effective_owner(
            session_state,
            &territory_name,
            next_owner.clone(),
            &live_owners,
        ) {
            let _ = session_state.undo_stack.pop();
        }
    });
}

fn reset_selected_owner_override(
    selected: RwSignal<Option<String>>,
    live_territories: RwSignal<ClientTerritoryMap>,
    session: RwSignal<Option<ClaimWorkingSession>>,
    active_owner: RwSignal<ClaimOwner>,
) {
    let Some(territory_name) = selected.get_untracked() else {
        return;
    };
    let live_snapshot = live_territories.get_untracked();
    let live_owners = current_live_owner_map(&live_snapshot);
    let Some(session_state) = session.get_untracked() else {
        return;
    };
    let base_owner = if session_state.follow_live {
        live_owners
            .get(&territory_name)
            .cloned()
            .unwrap_or_else(neutral_owner)
    } else {
        document_base_owner(&session_state.document, &territory_name)
    };
    session.update(|session_state| {
        let Some(session_state) = session_state.as_mut() else {
            return;
        };
        push_undo_state(session_state, &active_owner.get_untracked());
        if !set_effective_owner(
            session_state,
            &territory_name,
            base_owner.clone(),
            &live_owners,
        ) {
            let _ = session_state.undo_stack.pop();
        }
    });
}

fn apply_selected_resource_override(
    selected: RwSignal<Option<String>>,
    live_territories: RwSignal<ClientTerritoryMap>,
    session: RwSignal<Option<ClaimWorkingSession>>,
    active_owner: RwSignal<ClaimOwner>,
    field: &'static str,
    value: i32,
) {
    let Some(territory_name) = selected.get_untracked() else {
        return;
    };
    let live_snapshot = live_territories.get_untracked();
    session.update(|session_state| {
        let Some(session_state) = session_state.as_mut() else {
            return;
        };
        let mut next_resources =
            effective_resources_for_session(session_state, &live_snapshot, &territory_name);
        match field {
            "emeralds" => next_resources.emeralds = value,
            "ore" => next_resources.ore = value,
            "crops" => next_resources.crops = value,
            "fish" => next_resources.fish = value,
            "wood" => next_resources.wood = value,
            _ => return,
        }
        push_undo_state(session_state, &active_owner.get_untracked());
        if !set_effective_resources(
            session_state,
            &territory_name,
            next_resources,
            &live_snapshot,
        ) {
            let _ = session_state.undo_stack.pop();
        }
    });
}

fn reset_selected_resource_override(
    selected: RwSignal<Option<String>>,
    live_territories: RwSignal<ClientTerritoryMap>,
    session: RwSignal<Option<ClaimWorkingSession>>,
    active_owner: RwSignal<ClaimOwner>,
) {
    let Some(territory_name) = selected.get_untracked() else {
        return;
    };
    let live_snapshot = live_territories.get_untracked();
    let base_resources = base_resources_for_session(&live_snapshot, &territory_name);
    session.update(|session_state| {
        let Some(session_state) = session_state.as_mut() else {
            return;
        };
        push_undo_state(session_state, &active_owner.get_untracked());
        if !set_effective_resources(
            session_state,
            &territory_name,
            base_resources.clone(),
            &live_snapshot,
        ) {
            let _ = session_state.undo_stack.pop();
        }
    });
}

fn build_effective_client_map(
    session: Option<&ClaimWorkingSession>,
    live_territories: &ClientTerritoryMap,
) -> ClientTerritoryMap {
    let Some(session) = session else {
        return live_territories.clone();
    };
    let live_owners = current_live_owner_map(live_territories);
    let mut map = ClientTerritoryMap::with_capacity(live_territories.len());
    for (name, client_territory) in live_territories {
        let mut territory = client_territory.territory.clone();
        territory.guild = owner_to_guild(&effective_owner_for_session(session, &live_owners, name));
        territory.resources = effective_resources_for_session(session, live_territories, name);
        map.insert(
            name.clone(),
            ClientTerritory::from_territory(name, territory),
        );
    }
    map
}

fn canonical_document_for_session(
    session: &ClaimWorkingSession,
    live_territories: &ClientTerritoryMap,
    live_seq: u64,
    view: ClaimViewState,
) -> ClaimDocumentV1 {
    let live_owners = current_live_owner_map(live_territories);
    let mut document = session.document.clone();
    document.view = view;
    if session.follow_live {
        document.base = ClaimDocumentBase::FrozenLiveSnapshot {
            captured_at: Utc::now().to_rfc3339(),
            seq: live_seq,
            owners: live_owners.clone(),
        };
    }
    let territory_map = territory_map_from_client(live_territories);
    let mut compacted = HashMap::new();
    for territory in territory_map.keys() {
        let effective = effective_owner_for_session(session, &live_owners, territory);
        let base = document_base_owner(&document, territory);
        if effective != base {
            compacted.insert(territory.clone(), effective);
        }
    }
    document.overrides = compacted;
    if !session.follow_live {
        document.overrides = compact_claim_overrides(&document, &territory_map);
    }
    document
        .territory_state_overrides
        .retain(|territory, state| {
            if state.is_empty() || !territory_map.contains_key(territory) {
                return false;
            }
            let base_resources = territory_map
                .get(territory)
                .map(|territory| territory.resources.clone())
                .unwrap_or_default();
            state
                .resources
                .as_ref()
                .is_some_and(|resources| *resources != base_resources)
        });
    document
}

fn default_view_from(viewport: &Viewport, active_owner: &ClaimOwner) -> ClaimViewState {
    ClaimViewState {
        offset_x: viewport.offset_x,
        offset_y: viewport.offset_y,
        scale: viewport.scale,
        active_owner: Some(active_owner.clone()),
    }
}

fn slugify_name(input: &str) -> String {
    let slug: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

fn next_macro_id(name: &str) -> String {
    let slug = slugify_name(name);
    if slug.is_empty() {
        format!("macro-{}", Utc::now().timestamp_millis())
    } else {
        format!("{slug}-{}", Utc::now().timestamp_millis())
    }
}

#[cfg(test)]
fn encode_claim_fragment(document: &ClaimDocumentV1) -> Result<String, String> {
    let bytes = serde_json::to_vec(document).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_SHARE_FRAGMENT_BYTES {
        return Err("claim document is too large for URL sharing".to_string());
    }
    Ok(base64_url_encode(&bytes))
}

fn decode_claim_fragment(encoded: &str) -> Result<ClaimDocumentV1, String> {
    let bytes = base64_url_decode(encoded)?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
fn base64_url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut idx = 0;
    while idx + 3 <= bytes.len() {
        let chunk =
            ((bytes[idx] as u32) << 16) | ((bytes[idx + 1] as u32) << 8) | bytes[idx + 2] as u32;
        out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(chunk & 0x3f) as usize] as char);
        idx += 3;
    }
    match bytes.len() - idx {
        1 => {
            let chunk = (bytes[idx] as u32) << 16;
            out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        }
        2 => {
            let chunk = ((bytes[idx] as u32) << 16) | ((bytes[idx + 1] as u32) << 8);
            out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
            out.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        }
        _ => {}
    }
    out
}

fn base64_url_decode(encoded: &str) -> Result<Vec<u8>, String> {
    let mut value = 0u32;
    let mut bits = 0u8;
    let mut out = Vec::with_capacity((encoded.len() * 3) / 4);

    for byte in encoded.bytes() {
        let sextet = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err("invalid base64url fragment".to_string()),
        } as u32;

        value = (value << 6) | sextet;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((value >> bits) & 0xff) as u8);
        }
    }

    Ok(out)
}

fn copy_url_to_clipboard(url: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let navigator = window.navigator();
    let clipboard = navigator.clipboard();
    let promise = clipboard.write_text(url);
    spawn_local(async move {
        let _ = JsFuture::from(promise).await;
    });
}

fn data_url_for_json(json: &str) -> String {
    let encoded = js_sys::encode_uri_component(json)
        .as_string()
        .unwrap_or_else(|| json.to_string());
    format!("data:application/json;charset=utf-8,{encoded}")
}

fn read_local_draft() -> Option<StoredClaimDraft> {
    gloo_storage::LocalStorage::get(DRAFT_STORAGE_KEY).ok()
}

fn read_local_presets() -> Vec<StoredClaimPreset> {
    gloo_storage::LocalStorage::get(PRESET_STORAGE_KEY).unwrap_or_default()
}

fn read_macro_library() -> Vec<ClaimMacro> {
    gloo_storage::LocalStorage::get(MACRO_LIBRARY_STORAGE_KEY).unwrap_or_default()
}

fn take_startup_import_handoff() -> Option<StartupImportHandoff> {
    let handoff = gloo_storage::SessionStorage::get(IMPORT_HANDOFF_STORAGE_KEY).ok();
    gloo_storage::SessionStorage::delete(IMPORT_HANDOFF_STORAGE_KEY);
    handoff
}

fn staged_cache_is_fresh(version: u8, cached_at_ms: f64, max_age_ms: f64) -> bool {
    staged_cache_is_fresh_at(version, cached_at_ms, js_sys::Date::now(), max_age_ms)
}

fn staged_cache_is_fresh_at(version: u8, cached_at_ms: f64, now_ms: f64, max_age_ms: f64) -> bool {
    if version != BOOTSTRAP_STORAGE_VERSION {
        return false;
    }

    let age_ms = now_ms - cached_at_ms;
    age_ms.is_finite() && age_ms >= 0.0 && age_ms <= max_age_ms
}

fn stage_live_bootstrap(state: &LiveState) {
    let payload = StagedLiveBootstrap {
        version: BOOTSTRAP_STORAGE_VERSION,
        cached_at_ms: js_sys::Date::now(),
        state: state.clone(),
    };
    let _ = gloo_storage::SessionStorage::set(LIVE_BOOTSTRAP_STORAGE_KEY, &payload);
}

fn read_staged_live_bootstrap() -> Option<LiveState> {
    let staged: StagedLiveBootstrap =
        gloo_storage::SessionStorage::get(LIVE_BOOTSTRAP_STORAGE_KEY).ok()?;
    if staged_cache_is_fresh(
        staged.version,
        staged.cached_at_ms,
        LIVE_BOOTSTRAP_MAX_AGE_MS,
    ) {
        Some(staged.state)
    } else {
        None
    }
}

fn stage_geometry_bootstrap(geometry: &ClaimsBootstrapGeometry) {
    let payload = StagedGeometryBootstrap {
        version: BOOTSTRAP_STORAGE_VERSION,
        cached_at_ms: js_sys::Date::now(),
        geometry: geometry.clone(),
    };
    let _ = gloo_storage::SessionStorage::set(GEOMETRY_BOOTSTRAP_STORAGE_KEY, &payload);
}

fn read_staged_geometry_bootstrap() -> Option<ClaimsBootstrapGeometry> {
    let staged: StagedGeometryBootstrap =
        gloo_storage::SessionStorage::get(GEOMETRY_BOOTSTRAP_STORAGE_KEY).ok()?;
    if staged_cache_is_fresh(
        staged.version,
        staged.cached_at_ms,
        GEOMETRY_BOOTSTRAP_MAX_AGE_MS,
    ) {
        Some(staged.geometry)
    } else {
        None
    }
}

async fn fetch_claims_bootstrap_geometry() -> Result<ClaimsBootstrapGeometry, String> {
    let response = gloo_net::http::Request::get("/api/claims/bootstrap/geometry")
        .send()
        .await
        .map_err(|error| format!("fetch error: {error}"))?;

    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }

    response
        .json::<ClaimsBootstrapGeometry>()
        .await
        .map_err(|error| format!("parse error: {error}"))
}

async fn load_claims_bootstrap_geometry() -> Result<ClaimsBootstrapGeometry, String> {
    if let Some(geometry) = read_staged_geometry_bootstrap() {
        return Ok(geometry);
    }

    let geometry = fetch_claims_bootstrap_geometry().await?;
    stage_geometry_bootstrap(&geometry);
    Ok(geometry)
}

fn neutral_territory_from_geometry(geometry: &ClaimsTerritoryGeometry) -> Territory {
    Territory {
        guild: neutral_guild_ref(),
        acquired: Utc::now(),
        location: geometry.location.clone(),
        resources: geometry.resources.clone(),
        connections: geometry.connections.clone(),
        runtime: None,
    }
}

fn territory_map_from_geometry(geometry: &ClaimsBootstrapGeometry) -> TerritoryMap {
    geometry
        .territories
        .iter()
        .map(|(name, territory)| (name.clone(), neutral_territory_from_geometry(territory)))
        .collect()
}

fn client_map_from_geometry(geometry: &ClaimsBootstrapGeometry) -> ClientTerritoryMap {
    crate::territory::from_snapshot(territory_map_from_geometry(geometry))
}

fn validate_document_against_geometry(
    document: &ClaimDocumentV1,
    geometry: &ClaimsBootstrapGeometry,
) -> Result<(), ClaimValidationError> {
    validate_claim_document(document, geometry.territories.keys().map(String::as_str))
}

fn editor_init(
    geometry: ClaimsBootstrapGeometry,
    live_state: Option<LiveState>,
    document: ClaimDocumentV1,
    follow_live: bool,
    dirty: bool,
    selection: Vec<String>,
    source_snapshot_id: Option<String>,
    source_snapshot_url: Option<String>,
    active_owner: ClaimOwner,
) -> ClaimsEditorInit {
    ClaimsEditorInit {
        geometry,
        live_state,
        document,
        follow_live,
        dirty,
        selection,
        source_snapshot_id,
        source_snapshot_url,
        active_owner,
    }
}

fn hash_import_boot_payload(
    geometry: ClaimsBootstrapGeometry,
    live_state: Option<LiveState>,
    document: ClaimDocumentV1,
) -> ClaimsBootPayload {
    let active_owner = document
        .view
        .active_owner
        .clone()
        .unwrap_or_else(neutral_owner);
    ClaimsBootPayload::Import(editor_init(
        geometry,
        live_state,
        document,
        false,
        false,
        Vec::new(),
        None,
        None,
        active_owner,
    ))
}

fn apply_live_state(
    live_territories: RwSignal<ClientTerritoryMap>,
    live_seq: RwSignal<u64>,
    last_live_seq: RwSignal<Option<u64>>,
    live_bootstrap_pending: RwSignal<bool>,
    live_bootstrap_error: RwSignal<Option<String>>,
    state: LiveState,
) {
    stage_live_bootstrap(&state);
    live_seq.set(state.seq);
    last_live_seq.set(Some(state.seq));
    live_territories.set(crate::territory::from_snapshot(state.territories));
    live_bootstrap_pending.set(false);
    live_bootstrap_error.set(None);
}

fn validate_document_against_live(
    document: &ClaimDocumentV1,
    live_territories: &ClientTerritoryMap,
) -> Result<(), ClaimValidationError> {
    let territory_names: Vec<&str> = live_territories.keys().map(String::as_str).collect();
    validate_claim_document(document, territory_names)
}

fn apply_document_to_session(
    active_owner: RwSignal<ClaimOwner>,
    viewport: RwSignal<Viewport>,
    selected: RwSignal<Option<String>>,
    session: RwSignal<Option<ClaimWorkingSession>>,
    tab: RwSignal<ClaimTab>,
    status_message: RwSignal<Option<String>>,
    error_message: RwSignal<Option<String>>,
    document: ClaimDocumentV1,
    source_snapshot_id: Option<String>,
    source_snapshot_url: Option<String>,
    follow_live: bool,
) {
    let active = document
        .view
        .active_owner
        .clone()
        .unwrap_or_else(neutral_owner);
    active_owner.set(active);
    viewport.set(Viewport {
        offset_x: document.view.offset_x,
        offset_y: document.view.offset_y,
        scale: document.view.scale.max(0.05),
    });
    selected.set(None);
    session.set(Some(ClaimWorkingSession {
        document,
        follow_live,
        dirty: false,
        selection: Vec::new(),
        source_snapshot_id,
        source_snapshot_url,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
    }));
    tab.set(ClaimTab::Summary);
    status_message.set(None);
    error_message.set(None);
}

fn boot_route_title(route: &ClaimsRoute) -> &'static str {
    match route {
        ClaimsRoute::Root => "Claims Launcher Required",
        ClaimsRoute::NewBlank => "Booting Blank Claims Board",
        ClaimsRoute::NewLive => "Booting Live Snapshot",
        ClaimsRoute::Draft => "Recovering Local Draft",
        ClaimsRoute::Import => "Opening Imported Layout",
        ClaimsRoute::Saved(_) => "Loading Saved Snapshot",
    }
}

fn boot_route_copy(route: &ClaimsRoute) -> &'static str {
    match route {
        ClaimsRoute::Root => {
            "Claims route entry is launcher-driven now. Open the launcher to choose a session."
        }
        ClaimsRoute::NewBlank => {
            "Loading immutable territory geometry so the neutral board can open immediately."
        }
        ClaimsRoute::NewLive => {
            "Resolving territory geometry first, then freezing live ownership into a claims baseline."
        }
        ClaimsRoute::Draft => {
            "Loading local draft state and validating it against the latest geometry map."
        }
        ClaimsRoute::Import => {
            "Reading staged import data and opening the editor through the same geometry-first pipeline."
        }
        ClaimsRoute::Saved(_) => {
            "Loading geometry first so saved snapshot fetches never hold the page behind the static loader."
        }
    }
}

fn document_active_owner(document: &ClaimDocumentV1) -> ClaimOwner {
    document
        .view
        .active_owner
        .clone()
        .unwrap_or_else(neutral_owner)
}

fn live_owner_map_from_state(state: &LiveState) -> HashMap<String, ClaimOwner> {
    state
        .territories
        .iter()
        .map(|(name, territory)| (name.clone(), owner_from_live_guild(&territory.guild)))
        .collect()
}

async fn fetch_saved_claim_document(
    snapshot_id: &str,
) -> Result<SavedClaimDocumentResponse, String> {
    let url = format!("/api/claims/{snapshot_id}");
    let response = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|error| format!("fetch error: {error}"))?;

    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }

    response
        .json::<SavedClaimDocumentResponse>()
        .await
        .map_err(|error| format!("parse error: {error}"))
}

async fn create_saved_claim(document: ClaimDocumentV1) -> Result<SavedClaimResponse, String> {
    let request_body = serde_json::json!({
        "title": document.title.clone(),
        "document": document,
    });
    let request_body = serde_json::to_string(&request_body)
        .map_err(|error| format!("serialize error: {error}"))?;
    let request = gloo_net::http::Request::post("/api/claims")
        .header("Content-Type", "application/json")
        .body(request_body)
        .map_err(|_| "Failed to build save request".to_string())?;
    let response = request
        .send()
        .await
        .map_err(|_| "Failed to save snapshot".to_string())?;

    if !response.ok() {
        return Err(format!(
            "Failed to save snapshot (HTTP {})",
            response.status()
        ));
    }

    response
        .json::<SavedClaimResponse>()
        .await
        .map_err(|error| format!("parse error: {error}"))
}

fn absolute_claim_url(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }

    let origin = current_origin();
    if origin.is_empty() {
        url.to_string()
    } else {
        format!("{origin}{url}")
    }
}

#[cfg(target_arch = "wasm32")]
fn current_origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn current_origin() -> String {
    String::new()
}

fn saved_claim_url(snapshot_id: &str) -> String {
    absolute_claim_url(&format!("/claims/s/{snapshot_id}"))
}

fn reusable_share_url(session: &ClaimWorkingSession) -> Option<String> {
    if session.dirty {
        return None;
    }

    session
        .source_snapshot_url
        .clone()
        .or_else(|| session.source_snapshot_id.as_deref().map(saved_claim_url))
}

fn documents_match_for_saved_snapshot(
    current_document: &ClaimDocumentV1,
    saved_document: &ClaimDocumentV1,
) -> bool {
    let mut current_document = current_document.clone();
    let mut saved_document = saved_document.clone();
    current_document.view = ClaimViewState::default();
    saved_document.view = ClaimViewState::default();
    if let ClaimDocumentBase::FrozenLiveSnapshot { captured_at, .. } = &mut current_document.base {
        captured_at.clear();
    }
    if let ClaimDocumentBase::FrozenLiveSnapshot { captured_at, .. } = &mut saved_document.base {
        captured_at.clear();
    }
    current_document == saved_document
}

fn apply_saved_snapshot_if_current(
    session: RwSignal<Option<ClaimWorkingSession>>,
    live_territories: RwSignal<ClientTerritoryMap>,
    live_seq: RwSignal<u64>,
    viewport: RwSignal<Viewport>,
    active_owner: RwSignal<ClaimOwner>,
    snapshot_id: String,
    snapshot_url: String,
    saved_document: &ClaimDocumentV1,
) -> bool {
    let mut applied = false;
    session.update(|state| {
        if let Some(state) = state.as_mut() {
            let current_document = canonical_document_for_session(
                state,
                &live_territories.get_untracked(),
                live_seq.get_untracked(),
                default_view_from(&viewport.get_untracked(), &active_owner.get_untracked()),
            );
            if !documents_match_for_saved_snapshot(&current_document, saved_document) {
                return;
            }
            state.source_snapshot_id = Some(snapshot_id);
            state.source_snapshot_url = Some(snapshot_url);
            state.dirty = false;
            applied = true;
        }
    });
    applied
}

async fn resolve_boot_payload(
    route: ClaimsRoute,
    boot_status: RwSignal<String>,
) -> Result<ClaimsBootPayload, String> {
    boot_status.set("Loading territory geometry...".to_string());
    let geometry = load_claims_bootstrap_geometry().await?;

    match current_hash_document() {
        Ok(Some(document)) => {
            validate_document_against_geometry(&document, &geometry)
                .map_err(|error| format!("{error:?}"))?;
            return Ok(hash_import_boot_payload(
                geometry,
                read_staged_live_bootstrap(),
                document,
            ));
        }
        Ok(None) => {}
        Err(error) => return Err(error),
    }

    match route {
        ClaimsRoute::Root => {
            Err("Open /claims to choose a claims session from the launcher.".to_string())
        }
        ClaimsRoute::NewBlank => Ok(ClaimsBootPayload::Blank(editor_init(
            geometry,
            read_staged_live_bootstrap(),
            ClaimDocumentV1::blank(),
            false,
            false,
            Vec::new(),
            None,
            None,
            neutral_owner(),
        ))),
        ClaimsRoute::NewLive => {
            boot_status.set("Resolving live ownership snapshot...".to_string());
            let live_state = match read_staged_live_bootstrap() {
                Some(state) => state,
                None => history::fetch_live_state().await?,
            };
            stage_live_bootstrap(&live_state);
            let document = ClaimDocumentV1::frozen_live(
                None,
                live_state.seq,
                live_owner_map_from_state(&live_state),
            );
            Ok(ClaimsBootPayload::Live(editor_init(
                geometry,
                Some(live_state),
                document,
                false,
                false,
                Vec::new(),
                None,
                None,
                neutral_owner(),
            )))
        }
        ClaimsRoute::Draft => {
            boot_status.set("Loading local draft payload...".to_string());
            let draft = read_local_draft()
                .ok_or_else(|| "No local draft was found in this browser.".to_string())?;
            validate_document_against_geometry(&draft.document, &geometry)
                .map_err(|error| format!("{error:?}"))?;
            Ok(ClaimsBootPayload::Draft(editor_init(
                geometry,
                read_staged_live_bootstrap(),
                draft.document,
                draft.follow_live,
                draft.dirty,
                draft.selection,
                draft.source_snapshot_id,
                draft.source_snapshot_url,
                draft.active_owner,
            )))
        }
        ClaimsRoute::Import => {
            boot_status.set("Reading staged import handoff...".to_string());
            let handoff = take_startup_import_handoff().ok_or_else(|| {
                "No staged import was found. Start again from the claims launcher.".to_string()
            })?;
            validate_document_against_geometry(&handoff.document, &geometry)
                .map_err(|error| format!("{error:?}"))?;
            Ok(ClaimsBootPayload::Import(editor_init(
                geometry,
                read_staged_live_bootstrap(),
                handoff.document.clone(),
                handoff.follow_live,
                false,
                handoff.selection,
                handoff.source_snapshot_id,
                handoff.source_snapshot_url,
                document_active_owner(&handoff.document),
            )))
        }
        ClaimsRoute::Saved(snapshot_id) => {
            boot_status.set("Loading saved snapshot payload...".to_string());
            let payload = fetch_saved_claim_document(&snapshot_id).await?;
            validate_document_against_geometry(&payload.document, &geometry)
                .map_err(|error| format!("{error:?}"))?;
            Ok(ClaimsBootPayload::Saved(editor_init(
                geometry,
                read_staged_live_bootstrap(),
                payload.document.clone(),
                false,
                false,
                Vec::new(),
                Some(snapshot_id.clone()),
                Some(saved_claim_url(&snapshot_id)),
                document_active_owner(&payload.document),
            )))
        }
    }
}

fn trigger_import_picker(file_input_ref: NodeRef<html::Input>) {
    if let Some(input) = file_input_ref.get() {
        input.set_value("");
        input.click();
    }
}

#[component]
pub fn ClaimsPage(initial_path: String) -> impl IntoView {
    let route = parse_claims_route(&initial_path);
    let boot_payload: RwSignal<Option<ClaimsBootPayload>> = RwSignal::new(None);
    let boot_error: RwSignal<Option<String>> = RwSignal::new(None);
    let boot_status: RwSignal<String> = RwSignal::new(boot_route_copy(&route).to_string());
    let boot_started = RwSignal::new(false);
    let title = boot_route_title(&route).to_string();
    let copy = boot_route_copy(&route).to_string();

    Effect::new(move || {
        if boot_started.get_untracked() {
            return;
        }
        boot_started.set(true);
        let route = route.clone();
        spawn_local(async move {
            match resolve_boot_payload(route, boot_status).await {
                Ok(payload) => boot_payload.set(Some(payload)),
                Err(error) => boot_error.set(Some(error)),
            }
        });
    });

    view! {
        {move || {
            if let Some(payload) = boot_payload.get() {
                view! { <ClaimsEditor boot=payload /> }.into_any()
            } else {
                view! {
                    <div style="position: fixed; inset: 0; overflow: hidden; background:
                        radial-gradient(circle at 16% 14%, rgba(52,95,182,0.18), transparent 26%),
                        radial-gradient(circle at 84% 16%, rgba(245,197,66,0.10), transparent 24%),
                        radial-gradient(circle at 50% 100%, rgba(14,36,74,0.24), transparent 40%),
                        linear-gradient(180deg, #050a13 0%, #03060d 100%); color: #eef3ff;">
                        <div style="position: absolute; inset: 0; background-image:
                            linear-gradient(rgba(78,92,125,0.06) 1px, transparent 1px),
                            linear-gradient(90deg, rgba(78,92,125,0.06) 1px, transparent 1px);
                            background-size: 56px 56px; mask-image: linear-gradient(180deg, rgba(255,255,255,0.52), transparent 88%);
                            pointer-events: none;"></div>
                        <div style="position: relative; min-height: 100vh; display: grid; place-items: center; padding: 32px;">
                            <div style="width: min(620px, calc(100vw - 40px)); padding: 28px; border-radius: 28px;
                                border: 1px solid rgba(245,197,66,0.18);
                                background: linear-gradient(180deg, rgba(16,22,34,0.96), rgba(8,13,22,0.94));
                                box-shadow: 0 28px 80px rgba(0,0,0,0.42); display: grid; gap: 18px;">
                                <div style="display: flex; align-items: start; gap: 14px;">
                                    <div
                                        style:display=move || if boot_error.get().is_some() { "none" } else { "block" }
                                        style="width: 28px; height: 28px; border-radius: 999px; border: 3px solid rgba(245,197,66,0.22); border-top-color: #f5c542; animation: claims-bootstrap-spin 0.9s linear infinite;"
                                    ></div>
                                    <div style="display: grid; gap: 8px;">
                                        <div style="display: inline-flex; align-items: center; gap: 10px; padding: 8px 12px; width: fit-content;
                                            border-radius: 999px; border: 1px solid rgba(245,197,66,0.22);
                                            background: rgba(245,197,66,0.08); color: #f5c542; font-size: 0.7rem;
                                            letter-spacing: 0.12em; text-transform: uppercase;">
                                            "Claims Bootstrap"
                                        </div>
                                        <div style="font-family: 'Silkscreen', monospace; color: #f4c94b; font-size: clamp(1rem, 2vw, 1.2rem);">
                                            {title.clone()}
                                        </div>
                                        <div style="color: #a2afc8; font-size: 0.8rem; line-height: 1.8;">
                                            {copy.clone()}
                                        </div>
                                    </div>
                                </div>
                                <div style="padding: 16px 18px; border-radius: 18px; border: 1px solid rgba(92,109,150,0.22);
                                    background: rgba(11,16,26,0.72); color: #dbe3f6; font-size: 0.78rem; line-height: 1.8;">
                                    {move || boot_error.get().unwrap_or_else(|| boot_status.get())}
                                </div>
                                <div style="display: flex; gap: 12px; flex-wrap: wrap;">
                                    <a
                                        href="/claims"
                                        style="padding: 11px 14px; border-radius: 12px; border: 1px solid rgba(245,197,66,0.22);
                                            background: rgba(245,197,66,0.08); color: #f3ead0; text-decoration: none;"
                                    >
                                        "Open Claims Launcher"
                                    </a>
                                    <button
                                        type="button"
                                        style="padding: 11px 14px; border-radius: 12px; border: 1px solid #33405b;
                                            background: rgba(17,22,34,0.86); color: #dfe5f5; cursor: pointer;"
                                        on:click=move |_| {
                                            if let Some(window) = web_sys::window() {
                                                let _ = window.location().reload();
                                            }
                                        }
                                    >
                                        "Retry Route"
                                    </button>
                                </div>
                                <style>
                                    "@keyframes claims-bootstrap-spin { to { transform: rotate(360deg); } }"
                                </style>
                            </div>
                        </div>
                    </div>
                }
                .into_any()
            }
        }}
    }
}

#[component]
fn ClaimsEditor(boot: ClaimsBootPayload) -> impl IntoView {
    let ClaimsEditorInit {
        geometry,
        live_state: initial_live_state,
        document: initial_document,
        follow_live: initial_follow_live,
        dirty: initial_dirty,
        selection: initial_selection,
        source_snapshot_id: initial_source_snapshot_id,
        source_snapshot_url: initial_source_snapshot_url,
        active_owner: initial_active_owner,
    } = boot.into_editor_init();
    let initial_live_territories = initial_live_state
        .as_ref()
        .map(|state| crate::territory::from_snapshot(state.territories.clone()))
        .unwrap_or_else(|| client_map_from_geometry(&geometry));
    let initial_live_seq = initial_live_state
        .as_ref()
        .map(|state| state.seq)
        .unwrap_or(0);
    let initial_last_live_seq = initial_live_state.as_ref().map(|state| state.seq);
    let initial_session = ClaimWorkingSession {
        document: initial_document.clone(),
        follow_live: initial_follow_live,
        dirty: initial_dirty,
        selection: initial_selection,
        source_snapshot_id: initial_source_snapshot_id,
        source_snapshot_url: initial_source_snapshot_url,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
    };
    let initial_selected = selection_focus(&initial_session.selection, None);

    let live_territories: RwSignal<ClientTerritoryMap> =
        RwSignal::new(initial_live_territories.clone());
    let effective_territories: RwSignal<ClientTerritoryMap> = RwSignal::new(
        build_effective_client_map(Some(&initial_session), &initial_live_territories),
    );
    let live_seq: RwSignal<u64> = RwSignal::new(initial_live_seq);
    let session: RwSignal<Option<ClaimWorkingSession>> = RwSignal::new(Some(initial_session));
    let active_owner: RwSignal<ClaimOwner> = RwSignal::new(initial_active_owner);
    let tool: RwSignal<ClaimTool> = RwSignal::new(ClaimTool::View);
    let tab: RwSignal<ClaimTab> = RwSignal::new(ClaimTab::Territory);
    let editor_canvas_ready: RwSignal<bool> = RwSignal::new(false);
    let deferred_editor_work_ready: RwSignal<bool> = RwSignal::new(false);
    let snapshot_save_in_flight: RwSignal<bool> = RwSignal::new(false);
    let live_bootstrap_pending: RwSignal<bool> = RwSignal::new(initial_live_state.is_none());
    let live_bootstrap_error: RwSignal<Option<String>> = RwSignal::new(None);
    let error_message: RwSignal<Option<String>> = RwSignal::new(None);
    let status_message: RwSignal<Option<String>> = RwSignal::new(None);
    let claims_persistence_available: RwSignal<bool> = RwSignal::new(false);
    let preset_name_input: RwSignal<String> = RwSignal::new(String::new());
    let macro_name_input: RwSignal<String> = RwSignal::new(String::new());
    let guild_query: RwSignal<String> = RwSignal::new(String::new());
    let guild_results: RwSignal<Vec<GuildCatalogEntry>> = RwSignal::new(Vec::new());
    let guild_search_nonce: RwSignal<u64> = RwSignal::new(0);
    let local_presets: RwSignal<Vec<StoredClaimPreset>> = RwSignal::new(read_local_presets());
    let macro_library: RwSignal<Vec<ClaimMacro>> = RwSignal::new(read_macro_library());

    let viewport: RwSignal<Viewport> = RwSignal::new(Viewport {
        offset_x: initial_document.view.offset_x,
        offset_y: initial_document.view.offset_y,
        scale: initial_document.view.scale.max(0.05),
    });
    let hovered: RwSignal<Option<String>> = RwSignal::new(None);
    let selected: RwSignal<Option<String>> = RwSignal::new(initial_selected);
    let peek_territory: RwSignal<Option<String>> = RwSignal::new(None);
    let mouse_pos: RwSignal<(f64, f64)> = RwSignal::new((0.0, 0.0));
    let loaded_tiles: RwSignal<Vec<LoadedTile>> = RwSignal::new(Vec::new());
    let loaded_icons: RwSignal<Option<crate::icons::ResourceAtlas>> = RwSignal::new(None);
    let tick: RwSignal<i64> = RwSignal::new(Utc::now().timestamp());
    let is_mobile: RwSignal<bool> =
        RwSignal::new(canvas_dimensions().0 < crate::app::MOBILE_BREAKPOINT);
    let tile_fetch_scheduled: RwSignal<bool> = RwSignal::new(false);
    let resource_highlight: RwSignal<bool> = RwSignal::new(false);
    let show_resource_icons: RwSignal<bool> = RwSignal::new(false);
    let show_territory_ornaments: RwSignal<bool> = RwSignal::new(true);

    let current_mode: RwSignal<MapMode> = RwSignal::new(MapMode::Live);
    let connection: RwSignal<ConnectionStatus> = RwSignal::new(ConnectionStatus::Connecting);
    let history_timestamp: RwSignal<Option<i64>> = RwSignal::new(None);
    let history_buffered_updates: RwSignal<Vec<crate::app::BufferedUpdate>> =
        RwSignal::new(Vec::new());
    let history_buffer_mode_active: RwSignal<bool> = RwSignal::new(false);
    let history_buffer_size_max: RwSignal<usize> = RwSignal::new(0);
    let history_fetch_nonce: RwSignal<u64> = RwSignal::new(0);
    let last_live_seq: RwSignal<Option<u64>> = RwSignal::new(initial_last_live_seq);
    let needs_live_resync: RwSignal<bool> = RwSignal::new(false);
    let live_resync_in_flight: RwSignal<bool> = RwSignal::new(false);
    let sse_seq_gap_detected_count: RwSignal<u64> = RwSignal::new(0);
    let editor_stage_started: RwSignal<bool> = RwSignal::new(false);
    let background_live_bootstrap_started: RwSignal<bool> = RwSignal::new(false);
    let health_poll_started: RwSignal<bool> = RwSignal::new(false);

    if initial_live_state.is_none() {
        status_message.set(Some(LIVE_SYNC_PENDING_MESSAGE.to_string()));
    }

    provide_context(effective_territories);
    provide_context(viewport);
    provide_context(Hovered(hovered));
    provide_context(Selected(selected));
    provide_context(CurrentMode(current_mode));
    provide_context(HistoryTimestamp(history_timestamp));
    provide_context(IsMobile(is_mobile));
    provide_context(PeekTerritory(peek_territory));
    provide_context(DetailReturnGuild(RwSignal::new(None)));
    provide_context(mouse_pos);
    provide_context(loaded_tiles);
    provide_context(loaded_icons);
    provide_context(tick);
    provide_context(RwSignal::new(true));
    provide_context(AbbreviateNames(RwSignal::new(true)));
    provide_context(ShowCountdown(RwSignal::new(false)));
    provide_context(ShowGranularMapTime(RwSignal::new(false)));
    provide_context(ShowCompoundMapTime(RwSignal::new(false)));
    provide_context(ShowNames(RwSignal::new(false)));
    provide_context(ThickCooldownBorders(RwSignal::new(false)));
    provide_context(BoldConnections(RwSignal::new(true)));
    provide_context(ConnectionOpacityScale(RwSignal::new(0.35)));
    provide_context(ConnectionThicknessScale(RwSignal::new(0.7)));
    provide_context(ConnectionZoomFadeStart(RwSignal::new(0.10)));
    provide_context(ConnectionZoomFadeEnd(RwSignal::new(0.30)));
    provide_context(SuppressCooldownVisuals(RwSignal::new(true)));
    provide_context(FillAlphaBoost(RwSignal::new(0.12)));
    provide_context(ResourceHighlight(resource_highlight));
    provide_context(crate::app::ShowResourceIcons(show_resource_icons));
    provide_context(ShowTerritoryOrnaments(show_territory_ornaments));
    provide_context(ReadableFont(RwSignal::new(false)));
    provide_context(NameColorSetting(RwSignal::new(NameColor::Guild)));
    provide_context(TagColorSetting(RwSignal::new(NameColor::Guild)));
    provide_context(ShowMinimap(RwSignal::new(true)));
    provide_context(HeatModeEnabled(RwSignal::new(false)));
    provide_context(HeatEntriesByTerritory(RwSignal::new(HashMap::new())));
    provide_context(HeatMaxTakeCount(RwSignal::new(0)));
    provide_context(HeatWindowLabel(RwSignal::new(String::new())));
    provide_context(LabelScaleMaster(RwSignal::new(1.0)));
    provide_context(LabelScaleStatic(RwSignal::new(1.0)));
    provide_context(LabelScaleStaticName(RwSignal::new(1.0)));
    provide_context(LabelScaleDynamic(RwSignal::new(1.0)));
    provide_context(LabelScaleIcons(RwSignal::new(1.0)));
    provide_context(SidebarOpen(RwSignal::new(false)));
    provide_context(SidebarTransient(RwSignal::new(false)));
    provide_context(ShowSettings(RwSignal::new(false)));
    provide_context(HistoryBufferedUpdates(history_buffered_updates));
    provide_context(HistoryBufferModeActive(history_buffer_mode_active));
    provide_context(HistoryBufferSizeMax(history_buffer_size_max));
    provide_context(HistoryFetchNonce(history_fetch_nonce));
    provide_context(LastLiveSeq(last_live_seq));
    provide_context(NeedsLiveResync(needs_live_resync));
    provide_context(LiveResyncInFlight(live_resync_in_flight));
    provide_context(SseSeqGapDetectedCount(sse_seq_gap_detected_count));

    let apply_hit = Arc::new({
        move |territory_name: String, shift_held: bool| {
            let live_owners = current_live_owner_map(&live_territories.get_untracked());
            let current_active_owner = active_owner.get_untracked();
            session.update(|session_state| {
                let Some(session_state) = session_state.as_mut() else {
                    return;
                };
                match tool.get_untracked() {
                    ClaimTool::View => {}
                    ClaimTool::Paint => {
                        push_undo_state(session_state, &current_active_owner);
                        if !set_effective_owner(
                            session_state,
                            &territory_name,
                            current_active_owner.clone(),
                            &live_owners,
                        ) {
                            let _ = session_state.undo_stack.pop();
                        }
                    }
                    ClaimTool::EraseToNeutral => {
                        push_undo_state(session_state, &current_active_owner);
                        if !set_effective_owner(
                            session_state,
                            &territory_name,
                            ClaimOwner::Neutral,
                            &live_owners,
                        ) {
                            let _ = session_state.undo_stack.pop();
                        }
                    }
                    ClaimTool::Select => {
                        if shift_held {
                            if let Some(pos) = session_state
                                .selection
                                .iter()
                                .position(|n| n == &territory_name)
                            {
                                session_state.selection.remove(pos);
                            } else {
                                session_state.selection.push(territory_name.clone());
                            }
                        } else {
                            session_state.selection = vec![territory_name.clone()];
                        }
                        selected.set(selection_focus(
                            &session_state.selection,
                            Some(&territory_name),
                        ));
                        session_state.dirty = true;
                    }
                    ClaimTool::Eyedropper => {
                        active_owner.set(effective_owner_for_session(
                            session_state,
                            &live_owners,
                            &territory_name,
                        ));
                    }
                }
            });
            if tool.get_untracked() != ClaimTool::Select {
                selected.set(Some(territory_name));
            }
        }
    });
    let apply_box_select = Arc::new({
        move |territory_names: Vec<String>, shift_held: bool| {
            session.update(|session_state| {
                let Some(session_state) = session_state.as_mut() else {
                    return;
                };
                let next_selection = if shift_held {
                    let mut merged = session_state.selection.clone();
                    for name in &territory_names {
                        if !merged.contains(name) {
                            merged.push(name.clone());
                        }
                    }
                    merged
                } else {
                    territory_names.clone()
                };
                selected.set(selection_focus(
                    &next_selection,
                    territory_names.last().map(String::as_str),
                ));
                if session_state.selection != next_selection {
                    session_state.selection = next_selection;
                    session_state.dirty = true;
                }
            });
        }
    });
    provide_context(ClaimCanvasController {
        tool,
        handle_hit: apply_hit,
        handle_box_select: apply_box_select,
    });

    let metrics = Memo::new(move |_| {
        if !matches!(tab.get(), ClaimTab::Summary | ClaimTab::Compare) {
            return None;
        }
        let session = session.get()?;
        let live_map = live_territories.get();
        let territory_map = territory_map_from_client(&live_map);
        let document = canonical_document_for_session(
            &session,
            &live_map,
            live_seq.get(),
            session.document.view.clone(),
        );
        Some(compute_claim_metrics(&document, &territory_map))
    });

    let active_metrics = Memo::new(move |_| {
        let metrics = metrics.get()?;
        let active = owner_identity(&active_owner.get());
        metrics
            .guilds
            .into_iter()
            .find(|entry| owner_identity(&entry.owner) == active)
    });

    let selected_territory_details = Memo::new(move |_| {
        let territory_name = selected.get()?;
        let session_state = session.get()?;
        let effective_map = effective_territories.get();
        let effective = effective_map.get(&territory_name)?.territory.clone();
        let live_map = live_territories.get();
        let base = live_map.get(&territory_name)?.territory.clone();
        let live_owners = current_live_owner_map(&live_map);
        let effective_owner =
            effective_owner_for_session(&session_state, &live_owners, &territory_name);
        let base_owner = if session_state.follow_live {
            live_owners
                .get(&territory_name)
                .cloned()
                .unwrap_or_else(neutral_owner)
        } else {
            document_base_owner(&session_state.document, &territory_name)
        };
        let resources_overridden = session_state
            .document
            .territory_state_overrides
            .contains_key(&territory_name);
        Some((
            territory_name,
            effective,
            base,
            effective_owner,
            base_owner,
            resources_overridden,
        ))
    });

    // Auto-switch to Territory tab only when a new territory is selected in View mode,
    // not on every tab change (which would lock users out of other tabs).
    {
        let prev_selected: StoredValue<Option<String>> = StoredValue::new(None);
        Effect::new(move || {
            let sel = selected.get();
            let prev = prev_selected.get_value();
            if sel != prev {
                prev_selected.set_value(sel.clone());
                if sel.is_some() && tool.get() == ClaimTool::View {
                    tab.set(ClaimTab::Territory);
                }
            }
        });
    }

    Effect::new(move || {
        let session_value = session.get();
        let Some(session_value) = session_value else {
            return;
        };
        let live = live_territories.get();
        effective_territories.set(build_effective_client_map(Some(&session_value), &live));
    });

    Effect::new(move || {
        if editor_stage_started.get_untracked() {
            return;
        }
        editor_stage_started.set(true);
        spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(0)).await;
            editor_canvas_ready.set(true);
            gloo_timers::future::sleep(std::time::Duration::from_millis(0)).await;
            deferred_editor_work_ready.set(true);
        });
    });

    Effect::new(move || {
        if let Some(session_state) = session.get() {
            let mut document = session_state.document.clone();
            document.view = default_view_from(&viewport.get(), &active_owner.get());
            let draft = StoredClaimDraft {
                document,
                follow_live: session_state.follow_live,
                dirty: session_state.dirty,
                selection: session_state.selection.clone(),
                source_snapshot_id: session_state.source_snapshot_id.clone(),
                source_snapshot_url: session_state.source_snapshot_url.clone(),
                active_owner: active_owner.get(),
            };
            let _ = gloo_storage::LocalStorage::set(DRAFT_STORAGE_KEY, &draft);
        }
    });

    Effect::new(move || {
        let _ = gloo_storage::LocalStorage::set(PRESET_STORAGE_KEY, &local_presets.get());
    });

    Effect::new(move || {
        let _ = gloo_storage::LocalStorage::set(MACRO_LIBRARY_STORAGE_KEY, &macro_library.get());
    });

    Effect::new(move || {
        let query = guild_query.get();
        let trimmed = query.trim().to_string();
        if trimmed.is_empty() {
            guild_results.set(Vec::new());
            return;
        }
        let nonce = guild_search_nonce.get_untracked().wrapping_add(1);
        guild_search_nonce.set(nonce);
        spawn_local(async move {
            let encoded = js_sys::encode_uri_component(&trimmed)
                .as_string()
                .unwrap_or(trimmed.clone());
            let url = format!("/api/guilds/catalog?q={encoded}&limit=12");
            match gloo_net::http::Request::get(&url).send().await {
                Ok(response) if response.ok() => {
                    if let Ok(payload) = response.json::<GuildCatalogResponse>().await
                        && guild_search_nonce.get_untracked() == nonce
                    {
                        guild_results.set(payload.guilds);
                    }
                }
                _ => {}
            }
        });
    });

    Effect::new(move || {
        if !deferred_editor_work_ready.get()
            || session.get().is_none()
            || tile_fetch_scheduled.get_untracked()
        {
            return;
        }
        tile_fetch_scheduled.set(true);

        let Some(window) = web_sys::window() else {
            let (canvas_w, canvas_h) = canvas_dimensions();
            let context =
                tiles::TileFetchContext::new(viewport.get_untracked(), canvas_w, canvas_h);
            tiles::fetch_tiles(loaded_tiles, context);
            return;
        };

        let run_tile_fetch = {
            let window = window.clone();
            wasm_bindgen::closure::Closure::once(move || {
                let callback = wasm_bindgen::closure::Closure::once(move || {
                    let (canvas_w, canvas_h) = canvas_dimensions();
                    let context =
                        tiles::TileFetchContext::new(viewport.get_untracked(), canvas_w, canvas_h);
                    tiles::fetch_tiles(loaded_tiles, context);
                });

                let mut scheduled = false;
                if let Ok(idle_fn) =
                    Reflect::get(window.as_ref(), &JsValue::from_str("requestIdleCallback"))
                    && let Ok(idle_fn) = idle_fn.dyn_into::<Function>()
                {
                    let _ = idle_fn.call1(window.as_ref(), callback.as_ref().unchecked_ref());
                    scheduled = true;
                }
                if !scheduled {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        callback.as_ref().unchecked_ref(),
                        240,
                    );
                }
                callback.forget();
            })
        };

        let first_frame = wasm_bindgen::closure::Closure::once({
            let window = window.clone();
            move || {
                let _ = window.request_animation_frame(run_tile_fetch.as_ref().unchecked_ref());
                run_tile_fetch.forget();
            }
        });

        let _ = window.request_animation_frame(first_frame.as_ref().unchecked_ref());
        first_frame.forget();
    });

    on_cleanup(|| {
        sse::disconnect();
    });

    Effect::new(move || {
        if !deferred_editor_work_ready.get() {
            return;
        }
        sse::connect(live_territories, connection);
    });

    Effect::new(move || {
        if last_live_seq.get().is_some() {
            live_bootstrap_pending.set(false);
            live_bootstrap_error.set(None);
        }
    });

    Effect::new(move || {
        if live_bootstrap_pending.get() {
            return;
        }
        if status_message.get().as_deref() == Some(LIVE_SYNC_PENDING_MESSAGE) {
            status_message.set(None);
        }
    });

    Effect::new(move || {
        live_seq.set(last_live_seq.get().unwrap_or(0));
    });

    Effect::new(move || {
        if !deferred_editor_work_ready.get()
            || background_live_bootstrap_started.get_untracked()
            || last_live_seq.get_untracked().is_some()
        {
            return;
        }

        background_live_bootstrap_started.set(true);
        spawn_local(async move {
            loop {
                match history::fetch_live_state().await {
                    Ok(state) => {
                        apply_live_state(
                            live_territories,
                            live_seq,
                            last_live_seq,
                            live_bootstrap_pending,
                            live_bootstrap_error,
                            state,
                        );
                        break;
                    }
                    Err(error) => {
                        live_bootstrap_pending.set(true);
                        live_bootstrap_error
                            .set(Some(format!("Live territory bootstrap failed: {error}")));
                    }
                }

                gloo_timers::future::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    });

    Effect::new(move || {
        if health_poll_started.get_untracked() {
            return;
        }
        health_poll_started.set(true);
        spawn_local(async move {
            loop {
                match gloo_net::http::Request::get("/api/health").send().await {
                    Ok(response) if response.ok() => {
                        if let Ok(json) = response.json::<serde_json::Value>().await {
                            if let Some(value) = json
                                .get("claims_persistence_available")
                                .and_then(|value| value.as_bool())
                            {
                                claims_persistence_available.set(value);
                            }
                        }
                    }
                    _ => {}
                }

                gloo_timers::future::sleep(std::time::Duration::from_secs(20)).await;
            }
        });
    });

    let apply_active_to_selection = move |_| {
        let current_active_owner = active_owner.get_untracked();
        let live_owners = current_live_owner_map(&live_territories.get_untracked());
        session.update(|session_state| {
            let Some(session_state) = session_state.as_mut() else {
                return;
            };
            if session_state.selection.is_empty() {
                return;
            }
            push_undo_state(session_state, &current_active_owner);
            let mut changed = false;
            for territory in session_state.selection.clone() {
                changed |= set_effective_owner(
                    session_state,
                    &territory,
                    current_active_owner.clone(),
                    &live_owners,
                );
            }
            if !changed {
                let _ = session_state.undo_stack.pop();
            }
        });
    };

    let clear_selection = move |_| {
        selected.set(None);
        session.update(|session_state| {
            if let Some(session_state) = session_state.as_mut() {
                session_state.selection.clear();
            }
        });
    };

    let undo = move |_| {
        session.update(|session_state| {
            let Some(session_state) = session_state.as_mut() else {
                return;
            };
            let Some(previous) = session_state.undo_stack.pop() else {
                return;
            };
            session_state.redo_stack.push(snapshot_session(
                session_state,
                &active_owner.get_untracked(),
            ));
            session_state.document = previous.document;
            session_state.follow_live = previous.follow_live;
            session_state.selection = previous.selection;
            active_owner.set(previous.active_owner);
            selected.set(session_state.selection.last().cloned());
            session_state.dirty = true;
        });
    };

    let redo = move |_| {
        session.update(|session_state| {
            let Some(session_state) = session_state.as_mut() else {
                return;
            };
            let Some(next) = session_state.redo_stack.pop() else {
                return;
            };
            session_state.undo_stack.push(snapshot_session(
                session_state,
                &active_owner.get_untracked(),
            ));
            session_state.document = next.document;
            session_state.follow_live = next.follow_live;
            session_state.selection = next.selection;
            active_owner.set(next.active_owner);
            selected.set(session_state.selection.last().cloned());
            session_state.dirty = true;
        });
    };

    let freeze_now = move |_| {
        let Some(session_state) = session.get_untracked() else {
            return;
        };
        if !session_state.follow_live {
            return;
        }
        let document = canonical_document_for_session(
            &session_state,
            &live_territories.get_untracked(),
            live_seq.get_untracked(),
            default_view_from(&viewport.get_untracked(), &active_owner.get_untracked()),
        );
        session.update(|state| {
            if let Some(state) = state.as_mut() {
                push_undo_state(state, &active_owner.get_untracked());
                state.document = document.clone();
                state.follow_live = false;
            }
        });
    };

    let file_input_ref = NodeRef::<html::Input>::new();
    let file_input_change_ref = file_input_ref.clone();
    let on_file_change = move |_| {
        let Some(input) = file_input_change_ref.get() else {
            return;
        };
        let Some(files) = input.files() else {
            return;
        };
        let Some(file) = files.get(0) else {
            return;
        };
        let promise = file.text();
        spawn_local(async move {
            match JsFuture::from(promise).await {
                Ok(value) => {
                    let Some(text) = value.as_string() else {
                        error_message.set(Some("Import file was not valid text".to_string()));
                        return;
                    };
                    match serde_json::from_str::<ClaimDocumentV1>(&text) {
                        Ok(document) => {
                            if let Err(error) = validate_document_against_live(
                                &document,
                                &live_territories.get_untracked(),
                            ) {
                                error_message.set(Some(format!("{error:?}")));
                                return;
                            }
                            apply_document_to_session(
                                active_owner,
                                viewport,
                                selected,
                                session,
                                tab,
                                status_message,
                                error_message,
                                document,
                                None,
                                None,
                                false,
                            );
                        }
                        Err(error) => {
                            error_message.set(Some(error.to_string()));
                        }
                    }
                }
                Err(_) => error_message.set(Some("Failed to read import file".to_string())),
            }
        });
    };

    view! {
        <div style="position: relative; width: 100%; height: 100%; background: #0c0e17; color: #e6e3d9; overflow: hidden;">
            <input
                node_ref=file_input_ref
                type="file"
                accept=".json,application/json"
                style="display: none;"
                on:change=on_file_change
            />
            <MapCanvas />
            {move || {
                if !editor_canvas_ready.get() {
                    view! {
                        <div style="position: absolute; inset: 0; background:
                            radial-gradient(circle at 50% 28%, rgba(245,197,66,0.06), transparent 18%),
                            linear-gradient(180deg, rgba(12,14,23,0.92), rgba(7,10,17,0.96)); pointer-events: none;">
                            <div style="position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%);
                                padding: 12px 16px; border-radius: 999px; border: 1px solid rgba(245,197,66,0.18);
                                background: rgba(10,13,21,0.82); color: #cfd7ef; font-size: 0.74rem; letter-spacing: 0.04em;">
                                "Preparing campaign board..."
                            </div>
                        </div>
                    }
                    .into_any()
                } else {
                    ().into_any()
                }
            }}
            {move || {
                let Some(session_state) = session.get() else {
                    return ().into_any();
                };
                let vp = viewport.get();
                let territories = effective_territories.get();
                session_state
                    .selection
                    .iter()
                    .filter_map(|territory_name| {
                        let territory = territories.get(territory_name)?;
                        let region = &territory.territory.location;
                        let left = region.left() as f64;
                        let right = region.right() as f64;
                        let top = region.top() as f64;
                        let bottom = region.bottom() as f64;
                        let (sx1, sy1) = vp.world_to_screen(left, top);
                        let (sx2, sy2) = vp.world_to_screen(right, bottom);
                        let left_px = sx1.min(sx2);
                        let top_px = sy1.min(sy2);
                        let width_px = (sx2 - sx1).abs().max(1.0);
                        let height_px = (sy2 - sy1).abs().max(1.0);
                        Some(view! {
                            <div
                                style=format!(
                                    "position: absolute; left: {left_px}px; top: {top_px}px; width: {width_px}px; height: {height_px}px; z-index: 9; pointer-events: none; border: 1px solid rgba(245,197,66,0.95); box-shadow: 0 0 0 1px rgba(12,14,23,0.55) inset, 0 0 18px rgba(245,197,66,0.14); background: rgba(245,197,66,0.08);"
                                )
                            ></div>
                        })
                    })
                    .collect_view()
                    .into_any()
            }}
            {move || {
                if live_bootstrap_pending.get() {
                    view! {
                        <div style="position: absolute; top: 18px; left: 50%; transform: translateX(-50%); z-index: 20; padding: 10px 14px; border-radius: 999px; border: 1px solid rgba(245,197,66,0.2); background: rgba(12,16,25,0.92); color: #dfe5f5; font-size: 0.72rem; box-shadow: 0 16px 36px rgba(0,0,0,0.34);">
                            "Syncing live territory ownership in the background..."
                        </div>
                    }
                    .into_any()
                } else {
                    ().into_any()
                }
            }}
            {
                view! {
                        <div class="toolbar">
                            // Row 1: Tools + Undo/Redo | Guild picker
                            <div class="toolbar-row">
                                <div class="pill-group">
                                    {[
                                        ClaimTool::View,
                                        ClaimTool::Paint,
                                        ClaimTool::EraseToNeutral,
                                        ClaimTool::Select,
                                        ClaimTool::Eyedropper,
                                    ]
                                        .into_iter()
                                        .map(|entry| {
                                            view! {
                                                <button
                                                    class="btn"
                                                    class:active=move || tool.get() == entry
                                                    style="font-family: 'Silkscreen', monospace;"
                                                    title=entry.tooltip()
                                                    on:click=move |_| tool.set(entry)
                                                >
                                                    {entry.label()}
                                                </button>
                                            }
                                        })
                                        .collect_view()}
                                    <div class="pill-divider"></div>
                                    <button class="btn" title="Undo last action" on:click=undo>"Undo"</button>
                                    <button class="btn" title="Redo" on:click=redo>"Redo"</button>
                                </div>

                                <div class="pill-group" style="align-items: center; gap: 8px; padding: 10px 12px;">
                                    <button class="btn btn-neutral" title="Set active guild to neutral (unclaimed)" on:click=move |_| active_owner.set(neutral_owner())>
                                        "Neutral"
                                    </button>
                                    <input
                                        class="input"
                                        prop:value=move || guild_query.get()
                                        placeholder="Search guilds..."
                                        style="width: 220px;"
                                        on:input=move |event| guild_query.set(event_target_value(&event))
                                    />
                                    <div style="display: flex; gap: 6px; flex-wrap: wrap;">
                                        {move || guild_results.get().into_iter().map(|entry| {
                                            let owner = ClaimOwner::from_guild(GuildRef {
                                                uuid: entry.uuid.clone(),
                                                name: entry.name.clone(),
                                                prefix: entry.prefix.clone(),
                                                color: entry.color,
                                            });
                                            let label = format!("{} [{}]", entry.name, entry.prefix);
                                            view! {
                                                <button
                                                    class="btn btn-sm"
                                                    style="text-align: left;"
                                                    on:click=move |_| {
                                                        active_owner.set(owner.clone());
                                                        guild_query.set(String::new());
                                                        guild_results.set(Vec::new());
                                                    }
                                                >
                                                    {label}
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                </div>
                            </div>

                            // Row 2: Selection actions + toggles | badge
                            <div class="toolbar-row">
                                <div class="pill-group">
                                    {move || {
                                        let count = session.get().map(|state| state.selection.len()).unwrap_or(0);
                                        if count > 0 {
                                            view! {
                                                <button class="btn" on:click=clear_selection>"Clear"</button>
                                                <button class="btn" on:click=apply_active_to_selection>"Apply"</button>
                                                <div class="pill-divider"></div>
                                            }.into_any()
                                        } else {
                                            ().into_any()
                                        }
                                    }}
                                    <button
                                        class="btn"
                                        class:active=move || session.get().is_some_and(|session| session.follow_live)
                                        title="Toggle live territory sync"
                                        on:click=move |_| {
                                            session.update(|state| {
                                                if let Some(state) = state.as_mut() {
                                                    state.follow_live = !state.follow_live;
                                                    state.dirty = true;
                                                }
                                            });
                                        }
                                    >
                                        {move || if session.get().is_some_and(|session| session.follow_live) { "Live" } else { "Frozen" }}
                                    </button>
                                    <button
                                        class="btn"
                                        class:active=move || resource_highlight.get()
                                        title="Toggle resource highlight overlay"
                                        on:click=move |_| resource_highlight.update(|value| *value = !*value)
                                    >
                                        "Resources"
                                    </button>
                                </div>
                                <div class="badge">
                                    {move || format!("{} sel", session.get().map(|state| state.selection.len()).unwrap_or(0))}
                                </div>
                            </div>
                        </div>

                        <div class="sidebar-panel" style="position: absolute; top: 16px; right: 16px; bottom: 16px; width: min(360px, 34vw); z-index: 12;">
                            <div class="sidebar-tab-bar">
                                {[ClaimTab::Territory, ClaimTab::Summary, ClaimTab::Compare, ClaimTab::Macros, ClaimTab::Share]
                                    .into_iter()
                                    .map(|entry| {
                                        view! {
                                            <button
                                                class="btn btn-tab"
                                                class:active=move || tab.get() == entry
                                                on:click=move |_| tab.set(entry)
                                            >
                                                {entry.label()}
                                            </button>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                            <div class="sidebar-scroll">
                                {move || {
                                    if let Some(message) = error_message.get() {
                                        view! { <div style="padding: 10px; border-radius: 10px; background: rgba(190,72,72,0.16); border: 1px solid rgba(190,72,72,0.4); color: #ffcfcf;">{message}</div> }.into_any()
                                    } else if let Some(message) = live_bootstrap_error.get() {
                                        view! { <div style="padding: 10px; border-radius: 10px; background: rgba(190,72,72,0.16); border: 1px solid rgba(190,72,72,0.4); color: #ffcfcf;">{message}</div> }.into_any()
                                    } else if let Some(message) = status_message.get() {
                                        view! { <div style="padding: 10px; border-radius: 10px; background: rgba(112,170,92,0.14); border: 1px solid rgba(112,170,92,0.38); color: #d7ffd1;">{message}</div> }.into_any()
                                    } else {
                                        ().into_any()
                                    }
                                }}
                                {move || match tab.get() {
                        ClaimTab::Territory => {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 12px;">
                                    <div style="display: flex; align-items: center; justify-content: space-between; gap: 12px;">
                                        <div class="section-label">"Territory Inspector"</div>
                                        {move || {
                                            if session.get().is_some_and(|s| s.follow_live) {
                                                view! { <button class="btn btn-sm" on:click=freeze_now>"Freeze Now"</button> }.into_any()
                                            } else {
                                                ().into_any()
                                            }
                                        }}
                                    </div>
                                    {move || selected_territory_details.get().map(|(territory_name, effective, base, effective_owner, base_owner, resources_overridden)| {
                                        let coords = format!(
                                            "{}:{} -> {}:{}",
                                            effective.location.start[0],
                                            effective.location.start[1],
                                            effective.location.end[0],
                                            effective.location.end[1]
                                        );
                                        view! {
                                            <div class="card">
                                                <div style="display: flex; align-items: start; justify-content: space-between; gap: 10px;">
                                                    <div>
                                                        <div style="font-family: 'Silkscreen', monospace; color: #f4c94b; font-size: 0.88rem;">{territory_name.clone()}</div>
                                                        <div style="margin-top: 4px; color: #8d97b3; font-size: 0.72rem;">{coords}</div>
                                                    </div>
                                                    <button class="btn btn-sm" on:click=move |_| tool.set(ClaimTool::View)>"Focus View"</button>
                                                </div>
                                                <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 8px;">
                                                    <div class="card-inset">
                                                        <div style="color: #8d97b3; font-size: 0.68rem; text-transform: uppercase; letter-spacing: 0.08em;">"Current Owner"</div>
                                                        <div style="margin-top: 6px;">{effective_owner.display_name().to_string()}</div>
                                                    </div>
                                                    <div class="card-inset">
                                                        <div style="color: #8d97b3; font-size: 0.68rem; text-transform: uppercase; letter-spacing: 0.08em;">"Base Owner"</div>
                                                        <div style="margin-top: 6px;">{base_owner.display_name().to_string()}</div>
                                                    </div>
                                                </div>
                                                <div style="display: flex; gap: 8px; flex-wrap: wrap;">
                                                    <button
                                                        class="btn"
                                                        on:click=move |_| {
                                                            apply_selected_owner_override(
                                                                selected,
                                                                live_territories,
                                                                session,
                                                                active_owner,
                                                                active_owner.get_untracked(),
                                                            );
                                                        }
                                                    >
                                                        "Set To Active Guild"
                                                    </button>
                                                    <button
                                                        class="btn"
                                                        on:click=move |_| {
                                                            apply_selected_owner_override(
                                                                selected,
                                                                live_territories,
                                                                session,
                                                                active_owner,
                                                                ClaimOwner::Neutral,
                                                            );
                                                        }
                                                    >
                                                        "Set Neutral"
                                                    </button>
                                                    <button
                                                        class="btn"
                                                        on:click=move |_| {
                                                            reset_selected_owner_override(
                                                                selected,
                                                                live_territories,
                                                                session,
                                                                active_owner,
                                                            );
                                                        }
                                                    >
                                                        "Reset Owner"
                                                    </button>
                                                </div>
                                                <div style="display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 4px;">
                                                    <div class="section-label" style="font-size: 0.74rem;">"Resources"</div>
                                                    <div style="color: #8d97b3; font-size: 0.7rem;">
                                                        {if resources_overridden { "Custom resource stats" } else { "Using map defaults" }}
                                                    </div>
                                                </div>
                                                <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 8px;">
                                                    <label style="display: grid; gap: 4px;">
                                                        <span style="color: #8d97b3; font-size: 0.7rem;">"Emerald"</span>
                                                        <input class="input" type="number" min="0"
                                                            prop:value=effective.resources.emeralds.to_string()
                                                            on:input=move |event| {
                                                                let parsed = event_target_value(&event).trim().parse::<i32>().unwrap_or(0);
                                                                apply_selected_resource_override(selected, live_territories, session, active_owner, "emeralds", parsed.max(0));
                                                            }
                                                        />
                                                    </label>
                                                    <label style="display: grid; gap: 4px;">
                                                        <span style="color: #8d97b3; font-size: 0.7rem;">"Ore"</span>
                                                        <input class="input" type="number" min="0"
                                                            prop:value=effective.resources.ore.to_string()
                                                            on:input=move |event| {
                                                                let parsed = event_target_value(&event).trim().parse::<i32>().unwrap_or(0);
                                                                apply_selected_resource_override(selected, live_territories, session, active_owner, "ore", parsed.max(0));
                                                            }
                                                        />
                                                    </label>
                                                    <label style="display: grid; gap: 4px;">
                                                        <span style="color: #8d97b3; font-size: 0.7rem;">"Crops"</span>
                                                        <input class="input" type="number" min="0"
                                                            prop:value=effective.resources.crops.to_string()
                                                            on:input=move |event| {
                                                                let parsed = event_target_value(&event).trim().parse::<i32>().unwrap_or(0);
                                                                apply_selected_resource_override(selected, live_territories, session, active_owner, "crops", parsed.max(0));
                                                            }
                                                        />
                                                    </label>
                                                    <label style="display: grid; gap: 4px;">
                                                        <span style="color: #8d97b3; font-size: 0.7rem;">"Fish"</span>
                                                        <input class="input" type="number" min="0"
                                                            prop:value=effective.resources.fish.to_string()
                                                            on:input=move |event| {
                                                                let parsed = event_target_value(&event).trim().parse::<i32>().unwrap_or(0);
                                                                apply_selected_resource_override(selected, live_territories, session, active_owner, "fish", parsed.max(0));
                                                            }
                                                        />
                                                    </label>
                                                    <label style="display: grid; gap: 4px; grid-column: 1 / -1;">
                                                        <span style="color: #8d97b3; font-size: 0.7rem;">"Wood"</span>
                                                        <input class="input" type="number" min="0"
                                                            prop:value=effective.resources.wood.to_string()
                                                            on:input=move |event| {
                                                                let parsed = event_target_value(&event).trim().parse::<i32>().unwrap_or(0);
                                                                apply_selected_resource_override(selected, live_territories, session, active_owner, "wood", parsed.max(0));
                                                            }
                                                        />
                                                    </label>
                                                </div>
                                                <div style="display: flex; gap: 8px; flex-wrap: wrap;">
                                                    <button class="btn"
                                                        on:click=move |_| {
                                                            reset_selected_resource_override(
                                                                selected,
                                                                live_territories,
                                                                session,
                                                                active_owner,
                                                            );
                                                        }
                                                    >
                                                        "Reset Resources"
                                                    </button>
                                                    <div class="badge">
                                                        {format!("{} connections", base.connections.len())}
                                                    </div>
                                                </div>
                                            </div>
                                        }.into_any()
                                    }).unwrap_or_else(|| view! {
                                        <div class="card" style="color: #9aa6c4; line-height: 1.7;">
                                            "Click any territory to inspect its owner, resources, and connections."
                                        </div>
                                    }.into_any())}
                                </div>
                            }
                            .into_any()
                        }
                        ClaimTab::Summary => {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 10px;">
                                    <div class="section-label">"Active Guild"</div>
                                    <div class="card-inset" style="padding: 10px 12px;">
                                        {move || active_owner.get().display_name().to_string()}
                                    </div>
                                    {move || active_metrics.get().map(|metrics| {
                                        let top_conn = metrics.top_by_connections.as_ref()
                                            .map(|hub| format!("{} ({} conn)", hub.territory, hub.guild_connections))
                                            .unwrap_or_else(|| "None".to_string());
                                        let top_ext = metrics.top_by_externals.as_ref()
                                            .map(|hub| format!("{} ({} ext)", hub.territory, hub.externals))
                                            .unwrap_or_else(|| "None".to_string());
                                        view! {
                                            <div class="card" style="gap: 6px;">
                                                <div>{format!("Territories: {}", metrics.territory_count)}</div>
                                                <div>{format!("Changed vs base: {}", metrics.changed_territory_count)}</div>
                                                <div>{format!("Top connections: {top_conn}")}</div>
                                                <div>{format!("Top externals: {top_ext}")}</div>
                                                <div>{format!("Ore {} • Crops {} • Fish {} • Wood {}", metrics.resources.ore, metrics.resources.crops, metrics.resources.fish, metrics.resources.wood)}</div>
                                                <div>{format!("Doubles {} • Rainbow {} • Emerald {}", metrics.resources.any_double, metrics.resources.rainbow, metrics.resources.emerald)}</div>
                                            </div>
                                        }.into_any()
                                    }).unwrap_or_else(|| view! { <div style="color: #9a9590;">"Choose a guild or paint territories to see the summary."</div> }.into_any())}
                                </div>
                            }
                            .into_any()
                        }
                        ClaimTab::Compare => {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 8px;">
                                    {move || metrics.get().map(|metrics| {
                                        view! {
                                            <div class="card-inset">
                                                {format!("Total territories: {} • Neutral: {}", metrics.total_territories, metrics.neutral_territories)}
                                            </div>
                                            {metrics.guilds.into_iter().map(|entry| {
                                                let owner = entry.owner.clone();
                                                let label = format!(
                                                    "{} • {} terr • {} doubles • {} rainbow",
                                                    entry.owner.display_name(),
                                                    entry.territory_count,
                                                    entry.resources.any_double,
                                                    entry.resources.rainbow
                                                );
                                                view! {
                                                    <button class="btn" style="text-align: left;"
                                                        on:click=move |_| active_owner.set(owner.clone())
                                                    >
                                                        {label}
                                                    </button>
                                                }
                                            }).collect_view()}
                                        }.into_any()
                                    }).unwrap_or_else(|| view! { <div style="color: #9a9590;">"No active claim yet."</div> }.into_any())}
                                </div>
                            }
                            .into_any()
                        }
                        ClaimTab::Macros => {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 10px;">
                                    <div class="section-label">{move || format!("Selection ({})", session.get().map(|state| state.selection.len()).unwrap_or(0))}</div>
                                    <input class="input"
                                        prop:value=move || macro_name_input.get()
                                        placeholder="Macro name"
                                        on:input=move |event| macro_name_input.set(event_target_value(&event))
                                    />
                                    <button class="btn"
                                        on:click=move |_| {
                                            let name = macro_name_input.get_untracked().trim().to_string();
                                            if name.is_empty() {
                                                return;
                                            }
                                            session.update(|session_state| {
                                                let Some(session_state) = session_state.as_mut() else {
                                                    return;
                                                };
                                                let mut seen = BTreeSet::new();
                                                let territories: Vec<String> = session_state
                                                    .selection
                                                    .iter()
                                                    .filter(|territory| seen.insert((*territory).clone()))
                                                    .cloned()
                                                    .collect();
                                                if territories.is_empty() {
                                                    return;
                                                }
                                                let entry = ClaimMacro {
                                                    id: next_macro_id(&name),
                                                    name: name.clone(),
                                                    territories: territories.clone(),
                                                };
                                                session_state.document.macros.push(entry.clone());
                                                macro_library.update(|library| library.push(entry));
                                                session_state.dirty = true;
                                                macro_name_input.set(String::new());
                                            });
                                        }
                                    >
                                        "Save Macro From Selection"
                                    </button>
                                    <div class="section-label">"Layout Macros"</div>
                                    {move || session.get().map(|state| state.document.macros).unwrap_or_default().into_iter().map(|entry| {
                                        let select_macro = entry.territories.clone();
                                        let apply_macro = entry.territories.clone();
                                        let label = entry.name.clone();
                                        view! {
                                            <div class="card" style="flex-direction: row; align-items: center; padding: 8px;">
                                                <div style="flex: 1;">{label}</div>
                                                <button class="btn btn-sm"
                                                    on:click=move |_| {
                                                        let preferred = selected.get_untracked();
                                                        session.update(|state| {
                                                            if let Some(state) = state.as_mut() {
                                                                state.selection = select_macro.clone();
                                                            }
                                                        });
                                                        selected.set(selection_focus(
                                                            &select_macro,
                                                            preferred.as_deref(),
                                                        ));
                                                    }
                                                >
                                                    "Select"
                                                </button>
                                                <button class="btn btn-sm"
                                                    on:click=move |_| {
                                                        let current_active_owner = active_owner.get_untracked();
                                                        let live_owners = current_live_owner_map(&live_territories.get_untracked());
                                                        session.update(|state| {
                                                            if let Some(state) = state.as_mut() {
                                                                push_undo_state(state, &current_active_owner);
                                                                let mut changed = false;
                                                                for territory in &apply_macro {
                                                                    changed |= set_effective_owner(
                                                                        state,
                                                                        territory,
                                                                        current_active_owner.clone(),
                                                                        &live_owners,
                                                                    );
                                                                }
                                                                if !changed {
                                                                    let _ = state.undo_stack.pop();
                                                                }
                                                            }
                                                        });
                                                    }
                                                >
                                                    "Assign"
                                                </button>
                                            </div>
                                        }
                                    }).collect_view()}
                                    <div class="section-label">"Macro Library"</div>
                                    {move || macro_library.get().into_iter().map(|entry| {
                                        let territories = entry.territories.clone();
                                        let label = entry.name.clone();
                                        view! {
                                            <button class="btn" style="text-align: left;"
                                                on:click=move |_| {
                                                    session.update(|state| {
                                                        if let Some(state) = state.as_mut() {
                                                            state.selection = territories.clone();
                                                        }
                                                    });
                                                }
                                            >
                                                {label}
                                            </button>
                                        }
                                    }).collect_view()}
                                </div>
                            }
                            .into_any()
                        }
                        ClaimTab::Share => {
                            let import_input_ref = file_input_ref.clone();
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 10px;">
                                    <button class="btn"
                                        disabled=move || snapshot_save_in_flight.get()
                                        on:click=move |_| {
                                            if !claims_persistence_available.get_untracked() {
                                                error_message.set(Some("Short share URLs require server-side claims persistence on this deployment".to_string()));
                                                return;
                                            }
                                            if snapshot_save_in_flight.get_untracked() {
                                                return;
                                            }
                                            let Some(session_state) = session.get_untracked() else {
                                                return;
                                            };
                                            if let Some(url) = reusable_share_url(&session_state) {
                                                copy_url_to_clipboard(&url);
                                                status_message.set(Some("Copied short share URL".to_string()));
                                                return;
                                            }
                                            let document = canonical_document_for_session(
                                                &session_state,
                                                &live_territories.get_untracked(),
                                                live_seq.get_untracked(),
                                                default_view_from(&viewport.get_untracked(), &active_owner.get_untracked()),
                                            );
                                            let saved_document = document.clone();
                                            snapshot_save_in_flight.set(true);
                                            spawn_local(async move {
                                                let result = create_saved_claim(document).await;
                                                snapshot_save_in_flight.set(false);
                                                match result {
                                                    Ok(payload) => {
                                                        let share_url = absolute_claim_url(&payload.url);
                                                        if !apply_saved_snapshot_if_current(
                                                            session,
                                                            live_territories,
                                                            live_seq,
                                                            viewport,
                                                            active_owner,
                                                            payload.id.clone(),
                                                            share_url.clone(),
                                                            &saved_document,
                                                        ) {
                                                            status_message.set(Some(
                                                                "Snapshot finished saving, but newer edits are not included".to_string(),
                                                            ));
                                                            return;
                                                        }
                                                        copy_url_to_clipboard(&share_url);
                                                        status_message.set(Some("Copied short share URL".to_string()));
                                                    }
                                                    Err(error) => error_message.set(Some(error)),
                                                }
                                            });
                                        }
                                    >
                                        "Copy Short Share URL"
                                    </button>
                                    <button class="btn"
                                        disabled=move || snapshot_save_in_flight.get()
                                        on:click=move |_| {
                                            if snapshot_save_in_flight.get_untracked() {
                                                return;
                                            }
                                            let Some(session_state) = session.get_untracked() else {
                                                return;
                                            };
                                            if !claims_persistence_available.get_untracked() {
                                                status_message.set(Some("Server save is unavailable on this deployment".to_string()));
                                                return;
                                            }
                                            let document = canonical_document_for_session(
                                                &session_state,
                                                &live_territories.get_untracked(),
                                                live_seq.get_untracked(),
                                                default_view_from(&viewport.get_untracked(), &active_owner.get_untracked()),
                                            );
                                            let saved_document = document.clone();
                                            snapshot_save_in_flight.set(true);
                                            spawn_local(async move {
                                                let result = create_saved_claim(document).await;
                                                snapshot_save_in_flight.set(false);
                                                match result {
                                                    Ok(payload) => {
                                                        let share_url = absolute_claim_url(&payload.url);
                                                        if !apply_saved_snapshot_if_current(
                                                            session,
                                                            live_territories,
                                                            live_seq,
                                                            viewport,
                                                            active_owner,
                                                            payload.id.clone(),
                                                            share_url,
                                                            &saved_document,
                                                        ) {
                                                            status_message.set(Some(
                                                                format!(
                                                                    "Saved snapshot {} while newer edits remained unsaved",
                                                                    payload.id
                                                                ),
                                                            ));
                                                            return;
                                                        }
                                                        status_message
                                                            .set(Some(format!("Saved snapshot {}", payload.id)));
                                                    }
                                                    Err(error) => {
                                                        error_message.set(Some(error));
                                                    }
                                                }
                                            });
                                        }
                                    >
                                        "Save Immutable Snapshot"
                                    </button>
                                    <button class="btn"
                                        on:click=move |_| {
                                            let Some(session_state) = session.get_untracked() else {
                                                return;
                                            };
                                            let document = canonical_document_for_session(
                                                &session_state,
                                                &live_territories.get_untracked(),
                                                live_seq.get_untracked(),
                                                default_view_from(&viewport.get_untracked(), &active_owner.get_untracked()),
                                            );
                                            if let Ok(json) = serde_json::to_string_pretty(&document)
                                                && let Some(window) = web_sys::window()
                                                && let Some(document_node) = window.document()
                                                && let Ok(anchor) = document_node.create_element("a")
                                            {
                                                let _ = anchor.set_attribute("href", &data_url_for_json(&json));
                                                let _ = anchor.set_attribute("download", "sequoia-claim.json");
                                                if let Ok(anchor) = anchor.dyn_into::<web_sys::HtmlElement>() {
                                                    anchor.click();
                                                }
                                            }
                                        }
                                    >
                                        "Export JSON"
                                    </button>
                                    <button class="btn"
                                        on:click=move |_| trigger_import_picker(import_input_ref.clone())
                                    >
                                        "Import JSON"
                                    </button>
                                    <input class="input"
                                        prop:value=move || preset_name_input.get()
                                        placeholder="Local preset name"
                                        on:input=move |event| preset_name_input.set(event_target_value(&event))
                                    />
                                    <button class="btn"
                                        on:click=move |_| {
                                            let Some(session_state) = session.get_untracked() else {
                                                return;
                                            };
                                            let name = preset_name_input.get_untracked().trim().to_string();
                                            if name.is_empty() {
                                                return;
                                            }
                                            let document = canonical_document_for_session(
                                                &session_state,
                                                &live_territories.get_untracked(),
                                                live_seq.get_untracked(),
                                                default_view_from(&viewport.get_untracked(), &active_owner.get_untracked()),
                                            );
                                            local_presets.update(|presets| {
                                                presets.push(StoredClaimPreset {
                                                    id: format!("preset-{}", Utc::now().timestamp_millis()),
                                                    name: name.clone(),
                                                    document,
                                                });
                                            });
                                            preset_name_input.set(String::new());
                                            status_message.set(Some("Saved local preset".to_string()));
                                        }
                                    >
                                        "Save Local Preset"
                                    </button>
                                    <div style="display: flex; flex-direction: column; gap: 8px;">
                                        <div class="section-label">"Local Presets"</div>
                                        {move || local_presets.get().into_iter().map(|preset| {
                                            let document = preset.document.clone();
                                            let label = preset.name.clone();
                                            view! {
                                                <button class="btn" style="text-align: left;"
                                                    on:click=move |_| {
                                                        apply_document_to_session(
                                                            active_owner,
                                                            viewport,
                                                            selected,
                                                            session,
                                                            tab,
                                                            status_message,
                                                            error_message,
                                                            document.clone(),
                                                            None,
                                                            None,
                                                            false,
                                                        );
                                                    }
                                                >
                                                    {label}
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                </div>
                            }
                            .into_any()
                        }
                    }}
                            </div>
                        </div>
                }
            }

            {move || {
                if let Some(message) = live_bootstrap_error.get() {
                    view! {
                        <div style="position: absolute; left: 16px; bottom: 16px; z-index: 18; max-width: min(460px, calc(100vw - 32px)); padding: 12px 14px; border-radius: 14px; border: 1px solid rgba(255,139,128,0.28); background: rgba(58,18,20,0.82); color: #ffd1cb; box-shadow: 0 18px 42px rgba(0,0,0,0.36);">
                            {message}
                        </div>
                    }.into_any()
                } else {
                    ().into_any()
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claims_route_handles_root_and_saved_paths() {
        assert!(matches!(parse_claims_route("/claims"), ClaimsRoute::Root));
        assert!(matches!(
            parse_claims_route("/claims/new/import"),
            ClaimsRoute::Import
        ));
        assert!(matches!(
            parse_claims_route("/claims/s/test-123"),
            ClaimsRoute::Saved(id) if id == "test-123"
        ));
    }

    #[test]
    fn claim_fragment_round_trip_preserves_document() {
        let mut document = ClaimDocumentV1::blank();
        document.title = Some("Test".to_string());
        document.overrides.insert(
            "Ragni".to_string(),
            ClaimOwner::from_guild(GuildRef {
                uuid: "guild-1".to_string(),
                name: "Alpha".to_string(),
                prefix: "ALP".to_string(),
                color: Some((10, 20, 30)),
            }),
        );

        let encoded = encode_claim_fragment(&document).expect("encode fragment");
        let decoded = decode_claim_fragment(&encoded).expect("decode fragment");
        assert_eq!(decoded, document);
    }

    #[test]
    fn base64_url_round_trip_handles_short_inputs() {
        for input in [
            b"a".as_slice(),
            b"ab".as_slice(),
            b"abc".as_slice(),
            b"abcd".as_slice(),
        ] {
            let encoded = base64_url_encode(input);
            let decoded = base64_url_decode(&encoded).expect("decode");
            assert_eq!(decoded, input);
        }
    }

    #[test]
    fn reusable_share_url_prefers_stored_url_and_requires_clean_saved_session() {
        let session = ClaimWorkingSession {
            document: ClaimDocumentV1::blank(),
            follow_live: false,
            dirty: false,
            selection: Vec::new(),
            source_snapshot_id: Some("snapshot-123".to_string()),
            source_snapshot_url: Some("https://example.test/custom/snapshot-123".to_string()),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        };
        assert_eq!(
            reusable_share_url(&session),
            Some("https://example.test/custom/snapshot-123".to_string())
        );

        let fallback_session = ClaimWorkingSession {
            source_snapshot_url: None,
            ..session.clone()
        };
        assert_eq!(
            reusable_share_url(&fallback_session),
            Some("/claims/s/snapshot-123".to_string())
        );

        let dirty_session = ClaimWorkingSession {
            dirty: true,
            ..session
        };
        assert_eq!(reusable_share_url(&dirty_session), None);
    }

    #[test]
    fn documents_match_for_saved_snapshot_ignores_captured_at() {
        let owners = HashMap::from([("Ragni".to_string(), neutral_owner())]);
        let current_document = ClaimDocumentV1::frozen_live(None, 42, owners.clone());
        let mut saved_document = ClaimDocumentV1::frozen_live(None, 42, owners);
        if let ClaimDocumentBase::FrozenLiveSnapshot { captured_at, .. } = &mut saved_document.base
        {
            *captured_at = "2000-01-01T00:00:00Z".to_string();
        }
        assert!(documents_match_for_saved_snapshot(
            &current_document,
            &saved_document
        ));
    }

    #[test]
    fn documents_match_for_saved_snapshot_ignores_view() {
        let mut current_document = ClaimDocumentV1::blank();
        current_document.view = ClaimViewState {
            offset_x: 120.0,
            offset_y: -45.0,
            scale: 0.9,
            active_owner: Some(neutral_owner()),
        };
        let mut saved_document = current_document.clone();
        saved_document.view = ClaimViewState {
            offset_x: -80.0,
            offset_y: 300.0,
            scale: 0.15,
            active_owner: Some(ClaimOwner::from_guild(GuildRef {
                uuid: "guild-1".to_string(),
                name: "Alpha".to_string(),
                prefix: "ALP".to_string(),
                color: Some((10, 20, 30)),
            })),
        };
        assert!(documents_match_for_saved_snapshot(
            &current_document,
            &saved_document
        ));
    }

    #[test]
    fn stored_claim_draft_defaults_dirty_for_legacy_data() {
        let draft = serde_json::from_value::<StoredClaimDraft>(serde_json::json!({
            "document": ClaimDocumentV1::blank(),
            "follow_live": false,
            "selection": [],
            "source_snapshot_id": "snapshot-123",
            "active_owner": neutral_owner(),
        }))
        .expect("deserialize legacy draft");
        assert!(draft.dirty);
        assert_eq!(draft.source_snapshot_id.as_deref(), Some("snapshot-123"));
        assert_eq!(draft.source_snapshot_url, None);
    }

    #[test]
    fn staged_cache_freshness_requires_matching_version_and_age() {
        assert!(staged_cache_is_fresh_at(
            BOOTSTRAP_STORAGE_VERSION,
            5_000.0,
            5_500.0,
            1_000.0,
        ));
        assert!(!staged_cache_is_fresh_at(
            BOOTSTRAP_STORAGE_VERSION + 1,
            5_000.0,
            5_500.0,
            1_000.0,
        ));
        assert!(!staged_cache_is_fresh_at(
            BOOTSTRAP_STORAGE_VERSION,
            5_000.0,
            7_500.0,
            1_000.0,
        ));
    }

    #[test]
    fn hash_import_payload_routes_document_into_editor_init() {
        let geometry = ClaimsBootstrapGeometry {
            territories: HashMap::from([(
                "Ragni".to_string(),
                ClaimsTerritoryGeometry {
                    location: sequoia_shared::Region {
                        start: [0, 0],
                        end: [10, 10],
                    },
                    resources: Default::default(),
                    connections: Vec::new(),
                },
            )]),
        };
        let mut document = ClaimDocumentV1::blank();
        document.title = Some("Shared".to_string());

        let payload = hash_import_boot_payload(geometry.clone(), None, document.clone());
        let init = payload.into_editor_init();

        assert_eq!(init.geometry, geometry);
        assert_eq!(init.document, document);
        assert!(!init.dirty);
        assert!(!init.follow_live);
        assert!(init.selection.is_empty());
    }

    #[test]
    fn validate_document_against_geometry_uses_geometry_keys() {
        let geometry = ClaimsBootstrapGeometry {
            territories: HashMap::from([(
                "Ragni".to_string(),
                ClaimsTerritoryGeometry {
                    location: sequoia_shared::Region {
                        start: [0, 0],
                        end: [10, 10],
                    },
                    resources: Default::default(),
                    connections: Vec::new(),
                },
            )]),
        };
        let mut document = ClaimDocumentV1::blank();
        document
            .overrides
            .insert("Ragni".to_string(), ClaimOwner::Neutral);
        assert!(validate_document_against_geometry(&document, &geometry).is_ok());

        document
            .overrides
            .insert("Detlas".to_string(), ClaimOwner::Neutral);
        assert!(matches!(
            validate_document_against_geometry(&document, &geometry),
            Err(ClaimValidationError::UnknownTerritory(name)) if name == "Detlas"
        ));
    }
}
