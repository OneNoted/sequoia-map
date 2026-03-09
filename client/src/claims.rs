use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::Utc;
use gloo_storage::Storage;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use sequoia_shared::{
    ClaimDocumentBase, ClaimDocumentV1, ClaimMacro, ClaimOwner, ClaimValidationError,
    ClaimViewState, GuildRef, TerritoryMap, compact_claim_overrides, compute_claim_metrics,
    validate_claim_document,
};

use crate::app::{
    AbbreviateNames, BoldConnections, ConnectionOpacityScale, ConnectionThicknessScale,
    CurrentMode, DetailReturnGuild, HeatEntriesByTerritory, HeatMaxTakeCount, HeatModeEnabled,
    HeatWindowLabel, HistoryBufferModeActive, HistoryBufferSizeMax, HistoryBufferedUpdates,
    HistoryFetchNonce, HistoryTimestamp, Hovered, IsMobile, LabelScaleDynamic, LabelScaleIcons,
    LabelScaleMaster, LabelScaleStatic, LabelScaleStaticName, LastLiveSeq, LiveResyncInFlight,
    MapMode, NameColor, NameColorSetting, NeedsLiveResync, PeekTerritory, ReadableFont,
    ResourceHighlight, Selected, ShowCompoundMapTime, ShowCountdown, ShowGranularMapTime,
    ShowMinimap, ShowNames, ShowSettings, ShowTerritoryOrnaments, SidebarOpen, SidebarTransient,
    SseSeqGapDetectedCount, TagColorSetting, ThickCooldownBorders, canvas_dimensions,
    remove_loading_shell, set_loading_shell_step,
};
use crate::canvas::MapCanvas;
use crate::history;
use crate::sse::{self, ConnectionStatus};
use crate::territory::{ClientTerritory, ClientTerritoryMap};
use crate::tiles::{self, LoadedTile};
use crate::viewport::Viewport;

const DRAFT_STORAGE_KEY: &str = "sequoia_claim_draft_v1";
const PRESET_STORAGE_KEY: &str = "sequoia_claim_presets_v1";
const MACRO_LIBRARY_STORAGE_KEY: &str = "sequoia_claim_macros_v1";
const SHARE_FRAGMENT_PREFIX: &str = "#c=";
const MAX_SHARE_FRAGMENT_BYTES: usize = 120_000;

const NEUTRAL_GUILD_UUID: &str = "__neutral__";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClaimTool {
    Paint,
    EraseToNeutral,
    Select,
    Eyedropper,
}

impl ClaimTool {
    fn label(self) -> &'static str {
        match self {
            ClaimTool::Paint => "Paint",
            ClaimTool::EraseToNeutral => "Erase",
            ClaimTool::Select => "Select",
            ClaimTool::Eyedropper => "Pick",
        }
    }

    pub(crate) fn uses_canvas_edits(self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaimTab {
    Summary,
    Compare,
    Macros,
    Share,
}

impl ClaimTab {
    fn label(self) -> &'static str {
        match self {
            ClaimTab::Summary => "Summary",
            ClaimTab::Compare => "Compare",
            ClaimTab::Macros => "Macros",
            ClaimTab::Share => "Share",
        }
    }
}

#[derive(Clone)]
pub struct ClaimCanvasController {
    pub tool: RwSignal<ClaimTool>,
    pub handle_hit: Arc<dyn Fn(String) + Send + Sync>,
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
    undo_stack: Vec<ClaimUndoState>,
    redo_stack: Vec<ClaimUndoState>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredClaimDraft {
    document: ClaimDocumentV1,
    follow_live: bool,
    selection: Vec<String>,
    source_snapshot_id: Option<String>,
    active_owner: ClaimOwner,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredClaimPreset {
    id: String,
    name: String,
    document: ClaimDocumentV1,
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
    Saved(String),
}

fn parse_claims_route(path: &str) -> ClaimsRoute {
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
        color: Some((88, 92, 108)),
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
) -> ClaimDocumentV1 {
    let live_owners = current_live_owner_map(live_territories);
    let mut document = session.document.clone();
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
    session: RwSignal<Option<ClaimWorkingSession>>,
    tab: RwSignal<ClaimTab>,
    status_message: RwSignal<Option<String>>,
    error_message: RwSignal<Option<String>>,
    document: ClaimDocumentV1,
    source_snapshot_id: Option<String>,
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
    session.set(Some(ClaimWorkingSession {
        document,
        follow_live,
        dirty: false,
        selection: Vec::new(),
        source_snapshot_id,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
    }));
    tab.set(ClaimTab::Summary);
    status_message.set(None);
    error_message.set(None);
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

    let live_territories: RwSignal<ClientTerritoryMap> = RwSignal::new(ClientTerritoryMap::new());
    let effective_territories: RwSignal<ClientTerritoryMap> =
        RwSignal::new(ClientTerritoryMap::new());
    let live_seq: RwSignal<u64> = RwSignal::new(0);
    let session: RwSignal<Option<ClaimWorkingSession>> = RwSignal::new(None);
    let active_owner: RwSignal<ClaimOwner> = RwSignal::new(neutral_owner());
    let tool: RwSignal<ClaimTool> = RwSignal::new(ClaimTool::Paint);
    let tab: RwSignal<ClaimTab> = RwSignal::new(ClaimTab::Summary);
    let is_ready: RwSignal<bool> = RwSignal::new(false);
    let is_loading_snapshot: RwSignal<bool> = RwSignal::new(false);
    let live_bootstrap_pending: RwSignal<bool> = RwSignal::new(true);
    let live_bootstrap_error: RwSignal<Option<String>> = RwSignal::new(None);
    let error_message: RwSignal<Option<String>> = RwSignal::new(None);
    let status_message: RwSignal<Option<String>> = RwSignal::new(None);
    let claims_persistence_available: RwSignal<bool> = RwSignal::new(false);
    let saved_snapshot_id_input: RwSignal<String> = RwSignal::new(String::new());
    let preset_name_input: RwSignal<String> = RwSignal::new(String::new());
    let macro_name_input: RwSignal<String> = RwSignal::new(String::new());
    let guild_query: RwSignal<String> = RwSignal::new(String::new());
    let guild_results: RwSignal<Vec<GuildCatalogEntry>> = RwSignal::new(Vec::new());
    let guild_search_nonce: RwSignal<u64> = RwSignal::new(0);
    let local_presets: RwSignal<Vec<StoredClaimPreset>> = RwSignal::new(read_local_presets());
    let macro_library: RwSignal<Vec<ClaimMacro>> = RwSignal::new(read_macro_library());

    let viewport: RwSignal<Viewport> = RwSignal::new(Viewport::default());
    let hovered: RwSignal<Option<String>> = RwSignal::new(None);
    let selected: RwSignal<Option<String>> = RwSignal::new(None);
    let peek_territory: RwSignal<Option<String>> = RwSignal::new(None);
    let mouse_pos: RwSignal<(f64, f64)> = RwSignal::new((0.0, 0.0));
    let loaded_tiles: RwSignal<Vec<LoadedTile>> = RwSignal::new(Vec::new());
    let loaded_icons: RwSignal<Option<crate::icons::ResourceAtlas>> = RwSignal::new(None);
    let tick: RwSignal<i64> = RwSignal::new(Utc::now().timestamp());
    let is_mobile: RwSignal<bool> =
        RwSignal::new(canvas_dimensions().0 < crate::app::MOBILE_BREAKPOINT);

    let current_mode: RwSignal<MapMode> = RwSignal::new(MapMode::Live);
    let connection: RwSignal<ConnectionStatus> = RwSignal::new(ConnectionStatus::Connecting);
    let history_timestamp: RwSignal<Option<i64>> = RwSignal::new(None);
    let history_buffered_updates: RwSignal<Vec<crate::app::BufferedUpdate>> =
        RwSignal::new(Vec::new());
    let history_buffer_mode_active: RwSignal<bool> = RwSignal::new(false);
    let history_buffer_size_max: RwSignal<usize> = RwSignal::new(0);
    let history_fetch_nonce: RwSignal<u64> = RwSignal::new(0);
    let last_live_seq: RwSignal<Option<u64>> = RwSignal::new(None);
    let needs_live_resync: RwSignal<bool> = RwSignal::new(false);
    let live_resync_in_flight: RwSignal<bool> = RwSignal::new(false);
    let sse_seq_gap_detected_count: RwSignal<u64> = RwSignal::new(0);

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
    provide_context(ShowCompoundMapTime(RwSignal::new(true)));
    provide_context(ShowNames(RwSignal::new(false)));
    provide_context(ThickCooldownBorders(RwSignal::new(true)));
    provide_context(BoldConnections(RwSignal::new(true)));
    provide_context(ConnectionOpacityScale(RwSignal::new(1.1)));
    provide_context(ConnectionThicknessScale(RwSignal::new(1.25)));
    provide_context(ResourceHighlight(RwSignal::new(true)));
    provide_context(crate::app::ShowResourceIcons(RwSignal::new(true)));
    provide_context(ShowTerritoryOrnaments(RwSignal::new(true)));
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
        move |territory_name: String| {
            selected.set(Some(territory_name.clone()));
            let live_owners = current_live_owner_map(&live_territories.get_untracked());
            let current_active_owner = active_owner.get_untracked();
            session.update(|session_state| {
                let Some(session_state) = session_state.as_mut() else {
                    return;
                };
                match tool.get_untracked() {
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
                        if !session_state.selection.contains(&territory_name) {
                            session_state.selection.push(territory_name);
                            session_state.dirty = true;
                        }
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
        }
    });
    provide_context(ClaimCanvasController {
        tool,
        handle_hit: apply_hit,
    });

    let metrics = Memo::new(move |_| {
        let live_map = live_territories.get();
        let session = session.get()?;
        let territory_map = territory_map_from_client(&live_map);
        let document = canonical_document_for_session(&session, &live_map, live_seq.get());
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

    Effect::new(move || {
        let live = live_territories.get();
        let session_value = session.get();
        effective_territories.set(build_effective_client_map(session_value.as_ref(), &live));
    });

    Effect::new(move || {
        if let Some(session_state) = session.get() {
            let mut document = session_state.document.clone();
            document.view = default_view_from(&viewport.get(), &active_owner.get());
            let draft = StoredClaimDraft {
                document,
                follow_live: session_state.follow_live,
                selection: session_state.selection.clone(),
                source_snapshot_id: session_state.source_snapshot_id.clone(),
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
        if !loaded_tiles.get().is_empty() || live_territories.get().is_empty() {
            return;
        }
        let (canvas_w, canvas_h) = canvas_dimensions();
        let context = tiles::TileFetchContext::new(viewport.get_untracked(), canvas_w, canvas_h);
        tiles::fetch_tiles(loaded_tiles, context);
    });

    Effect::new(move || {
        set_loading_shell_step("Starting claim editor");
        remove_loading_shell();
    });

    Effect::new(move || {
        sse::connect(live_territories, connection);
        on_cleanup(|| {
            sse::disconnect();
        });
    });

    Effect::new(move || {
        if !live_territories.get().is_empty() {
            live_bootstrap_pending.set(false);
            live_bootstrap_error.set(None);
        }
    });

    Effect::new(move || {
        if live_territories.get().is_empty() || is_ready.get_untracked() {
            return;
        }

        is_ready.set(true);

        match current_hash_document() {
            Ok(Some(document)) => {
                apply_document_to_session(
                    active_owner,
                    viewport,
                    session,
                    tab,
                    status_message,
                    error_message,
                    document,
                    None,
                    false,
                );
            }
            Ok(None) => {
                if let ClaimsRoute::Saved(snapshot_id) = &route {
                    is_loading_snapshot.set(true);
                    let snapshot_id = snapshot_id.clone();
                    spawn_local(async move {
                        let url = format!("/api/claims/{snapshot_id}");
                        match gloo_net::http::Request::get(&url).send().await {
                            Ok(response) if response.ok() => {
                                match response.json::<SavedClaimDocumentResponse>().await {
                                    Ok(payload) => {
                                        apply_document_to_session(
                                            active_owner,
                                            viewport,
                                            session,
                                            tab,
                                            status_message,
                                            error_message,
                                            payload.document,
                                            Some(payload.id),
                                            false,
                                        );
                                    }
                                    Err(error) => {
                                        error_message.set(Some(error.to_string()));
                                    }
                                }
                            }
                            _ => {
                                error_message
                                    .set(Some("Failed to load saved snapshot".to_string()));
                            }
                        }
                        is_loading_snapshot.set(false);
                    });
                }
            }
            Err(error) => {
                error_message.set(Some(error));
            }
        }
    });

    Effect::new(move || {
        live_seq.set(last_live_seq.get().unwrap_or(0));
    });

    spawn_local(async move {
        loop {
            if !live_territories.get_untracked().is_empty() {
                live_bootstrap_pending.set(false);
                live_bootstrap_error.set(None);
                break;
            }

            match history::fetch_live_state().await {
                Ok(state) => {
                    live_seq.set(state.seq);
                    last_live_seq.set(Some(state.seq));
                    live_territories.set(crate::territory::from_snapshot(state.territories));
                    live_bootstrap_pending.set(false);
                    live_bootstrap_error.set(None);
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
                                session,
                                tab,
                                status_message,
                                error_message,
                                document,
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

    let has_local_draft = Signal::derive(move || read_local_draft().is_some());

    view! {
        <div style="position: relative; width: 100%; height: 100%; background: #0c0e17; color: #e6e3d9; overflow: hidden;">
            {move || {
                if is_ready.get() && session.get().is_some() {
                    view! { <MapCanvas /> }.into_any()
                } else {
                    view! { <div style="position: absolute; inset: 0; background: #0c0e17;" /> }
                        .into_any()
                }
            }}
            <div style="position: absolute; top: 16px; left: 16px; z-index: 12; display: flex; gap: 10px; flex-wrap: wrap; max-width: min(92vw, 780px);">
                <div style="display: flex; gap: 6px; padding: 10px; background: rgba(19,22,31,0.94); border: 1px solid #2c3146; border-radius: 10px; box-shadow: 0 12px 32px rgba(0,0,0,0.35);">
                    {[
                        ClaimTool::Paint,
                        ClaimTool::EraseToNeutral,
                        ClaimTool::Select,
                        ClaimTool::Eyedropper,
                    ]
                        .into_iter()
                        .map(|entry| {
                            view! {
                                <button
                                    style:background=move || if tool.get() == entry { "#f5c542" } else { "#171b28" }
                                    style:color=move || if tool.get() == entry { "#161821" } else { "#d9d4c3" }
                                    style="padding: 8px 12px; border-radius: 8px; border: 1px solid #3a415c; font-family: 'Silkscreen', monospace; cursor: pointer;"
                                    on:click=move |_| tool.set(entry)
                                >
                                    {entry.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>

                <div style="display: flex; align-items: center; gap: 8px; padding: 10px 12px; background: rgba(19,22,31,0.94); border: 1px solid #2c3146; border-radius: 10px;">
                    <button
                        style="padding: 8px 10px; border-radius: 8px; border: 1px solid #45506e; background: #5b5f70; color: #f1eee6; cursor: pointer;"
                        on:click=move |_| active_owner.set(neutral_owner())
                    >
                        "Neutral"
                    </button>
                    <input
                        prop:value=move || guild_query.get()
                        placeholder="Search guilds"
                        style="width: 220px; padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #111521; color: #e6e3d9;"
                        on:input=move |event| guild_query.set(event_target_value(&event))
                    />
                    <div style="display: flex; gap: 6px; flex-wrap: wrap; max-width: 240px;">
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
                                    style="padding: 6px 8px; border-radius: 8px; border: 1px solid #36405d; background: #171b28; color: #e6e3d9; cursor: pointer; text-align: left;"
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

                <div style="display: flex; gap: 6px; padding: 10px; background: rgba(19,22,31,0.94); border: 1px solid #2c3146; border-radius: 10px;">
                    <button style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;" on:click=undo>"Undo"</button>
                    <button style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;" on:click=redo>"Redo"</button>
                    <button style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;" on:click=clear_selection>"Clear Selection"</button>
                    <button style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;" on:click=apply_active_to_selection>"Apply To Selection"</button>
                    <button
                        style:background=move || if session.get().is_some_and(|session| session.follow_live) { "#f5c542" } else { "#171b28" }
                        style:color=move || if session.get().is_some_and(|session| session.follow_live) { "#161821" } else { "#e6e3d9" }
                        style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; cursor: pointer;"
                        on:click=move |_| {
                            session.update(|state| {
                                if let Some(state) = state.as_mut() {
                                    state.follow_live = !state.follow_live;
                                    state.dirty = true;
                                }
                            });
                        }
                    >
                        {move || if session.get().is_some_and(|session| session.follow_live) { "Live Follow" } else { "Frozen" }}
                    </button>
                    <button style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;" on:click=freeze_now>"Freeze Now"</button>
                </div>
            </div>

            <div style="position: absolute; top: 16px; right: 16px; bottom: 16px; width: min(360px, 34vw); z-index: 12; display: flex; flex-direction: column; background: rgba(19,22,31,0.96); border: 1px solid #2c3146; border-radius: 14px; box-shadow: 0 18px 40px rgba(0,0,0,0.4); overflow: hidden;">
                <div style="display: flex; gap: 6px; padding: 12px; border-bottom: 1px solid #2c3146; background: rgba(14,16,24,0.9);">
                    {[ClaimTab::Summary, ClaimTab::Compare, ClaimTab::Macros, ClaimTab::Share]
                        .into_iter()
                        .map(|entry| {
                            view! {
                                <button
                                    style:background=move || if tab.get() == entry { "#f5c542" } else { "#171b28" }
                                    style:color=move || if tab.get() == entry { "#161821" } else { "#d9d4c3" }
                                    style="flex: 1; padding: 8px 10px; border-radius: 8px; border: 1px solid #36405d; cursor: pointer; font-family: 'Silkscreen', monospace; font-size: 0.72rem;"
                                    on:click=move |_| tab.set(entry)
                                >
                                    {entry.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <div style="flex: 1; overflow-y: auto; padding: 14px 16px; display: flex; flex-direction: column; gap: 12px;">
                    <input
                        node_ref=file_input_ref
                        type="file"
                        accept=".json,application/json"
                        style="display: none;"
                        on:change=on_file_change
                    />
                    {move || {
                        if let Some(message) = error_message.get() {
                            view! { <div style="padding: 10px; border-radius: 10px; background: rgba(190,72,72,0.16); border: 1px solid rgba(190,72,72,0.4); color: #ffcfcf;">{message}</div> }.into_any()
                        } else if let Some(message) = status_message.get() {
                            view! { <div style="padding: 10px; border-radius: 10px; background: rgba(112,170,92,0.14); border: 1px solid rgba(112,170,92,0.38); color: #d7ffd1;">{message}</div> }.into_any()
                        } else {
                            ().into_any()
                        }
                    }}
                    {move || match tab.get() {
                        ClaimTab::Summary => {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 10px;">
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542; font-size: 0.78rem;">"Active Guild"</div>
                                    <div style="padding: 10px 12px; border-radius: 10px; background: #121725; border: 1px solid #2c3146;">
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
                                            <div style="padding: 12px; border-radius: 12px; background: #121725; border: 1px solid #2c3146; display: flex; flex-direction: column; gap: 6px;">
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
                                            <div style="padding: 10px; border-radius: 10px; background: #121725; border: 1px solid #2c3146;">
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
                                                    <button
                                                        style="padding: 10px; border-radius: 10px; border: 1px solid #2c3146; background: #121725; color: #e6e3d9; cursor: pointer; text-align: left;"
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
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">{move || format!("Selection ({})", session.get().map(|state| state.selection.len()).unwrap_or(0))}</div>
                                    <input
                                        prop:value=move || macro_name_input.get()
                                        placeholder="Macro name"
                                        style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #111521; color: #e6e3d9;"
                                        on:input=move |event| macro_name_input.set(event_target_value(&event))
                                    />
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
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
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">"Layout Macros"</div>
                                    {move || session.get().map(|state| state.document.macros).unwrap_or_default().into_iter().map(|entry| {
                                        let select_macro = entry.territories.clone();
                                        let apply_macro = entry.territories.clone();
                                        let label = entry.name.clone();
                                        view! {
                                            <div style="display: flex; gap: 6px; align-items: center; padding: 8px; border: 1px solid #2c3146; border-radius: 8px; background: #121725;">
                                                <div style="flex: 1;">{label}</div>
                                                <button
                                                    style="padding: 6px 8px; border-radius: 8px; border: 1px solid #36405d; background: #171b28; color: #e6e3d9; cursor: pointer;"
                                                    on:click=move |_| {
                                                        session.update(|state| {
                                                            if let Some(state) = state.as_mut() {
                                                                state.selection = select_macro.clone();
                                                            }
                                                        });
                                                    }
                                                >
                                                    "Select"
                                                </button>
                                                <button
                                                    style="padding: 6px 8px; border-radius: 8px; border: 1px solid #36405d; background: #171b28; color: #e6e3d9; cursor: pointer;"
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
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">"Macro Library"</div>
                                    {move || macro_library.get().into_iter().map(|entry| {
                                        let territories = entry.territories.clone();
                                        let label = entry.name.clone();
                                        view! {
                                            <button
                                                style="padding: 9px 10px; border-radius: 8px; border: 1px solid #36405d; background: #121725; color: #e6e3d9; cursor: pointer; text-align: left;"
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
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
                                        on:click=move |_| {
                                            let Some(session_state) = session.get_untracked() else {
                                                return;
                                            };
                                            let document = canonical_document_for_session(
                                                &session_state,
                                                &live_territories.get_untracked(),
                                                live_seq.get_untracked(),
                                            );
                                            match encode_claim_fragment(&document) {
                                                Ok(fragment) => {
                                                    if let Some(window) = web_sys::window() {
                                                        let origin = window.location().origin().unwrap_or_default();
                                                        let url = format!("{origin}/claims{SHARE_FRAGMENT_PREFIX}{fragment}");
                                                        copy_url_to_clipboard(&url);
                                                        status_message.set(Some("Copied share URL".to_string()));
                                                    }
                                                }
                                                Err(error) => error_message.set(Some(error)),
                                            }
                                        }
                                    >
                                        "Copy Share URL"
                                    </button>
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
                                        on:click=move |_| {
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
                                            );
                                            spawn_local(async move {
                                                let request_body = serde_json::json!({
                                                    "title": document.title.clone(),
                                                    "document": document,
                                                });
                                                let request = gloo_net::http::Request::post("/api/claims")
                                                    .header("Content-Type", "application/json")
                                                    .body(serde_json::to_string(&request_body).unwrap_or_default());

                                                match request {
                                                    Ok(request) => match request.send().await {
                                                        Ok(response) if response.ok() => {
                                                            if let Ok(payload) = response.json::<SavedClaimResponse>().await {
                                                                status_message.set(Some(format!("Saved snapshot {}", payload.id)));
                                                            }
                                                        }
                                                        _ => error_message.set(Some("Failed to save snapshot".to_string())),
                                                    },
                                                    Err(_) => error_message.set(Some("Failed to build save request".to_string())),
                                                }
                                            });
                                        }
                                    >
                                        "Save Immutable Snapshot"
                                    </button>
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
                                        on:click=move |_| {
                                            let Some(session_state) = session.get_untracked() else {
                                                return;
                                            };
                                            let document = canonical_document_for_session(
                                                &session_state,
                                                &live_territories.get_untracked(),
                                                live_seq.get_untracked(),
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
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
                                        on:click=move |_| trigger_import_picker(import_input_ref.clone())
                                    >
                                        "Import JSON"
                                    </button>
                                    <input
                                        prop:value=move || preset_name_input.get()
                                        placeholder="Local preset name"
                                        style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #111521; color: #e6e3d9;"
                                        on:input=move |event| preset_name_input.set(event_target_value(&event))
                                    />
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
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
                                        <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">"Local Presets"</div>
                                        {move || local_presets.get().into_iter().map(|preset| {
                                            let document = preset.document.clone();
                                            let label = preset.name.clone();
                                            view! {
                                                <button
                                                    style="padding: 9px 10px; border-radius: 8px; border: 1px solid #36405d; background: #121725; color: #e6e3d9; cursor: pointer; text-align: left;"
                                                    on:click=move |_| {
                                                        apply_document_to_session(
                                                            active_owner,
                                                            viewport,
                                                            session,
                                                            tab,
                                                            status_message,
                                                            error_message,
                                                            document.clone(),
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

            {move || {
                if is_loading_snapshot.get() {
                    view! {
                        <div style="position: absolute; inset: 0; z-index: 18; display: flex; align-items: center; justify-content: center; background: rgba(10,12,19,0.72);">
                            <div style="padding: 14px 18px; border-radius: 12px; background: #121725; border: 1px solid #2c3146;">
                                "Loading saved snapshot…"
                            </div>
                        </div>
                    }.into_any()
                } else if session.get().is_none() {
                    let import_input_ref = file_input_ref.clone();
                    view! {
                        <div style="position: absolute; inset: 0; z-index: 20; display: flex; align-items: center; justify-content: center; background: rgba(10,12,19,0.90);">
                            <div style="width: min(880px, 92vw); padding: 24px; border-radius: 18px; background: linear-gradient(180deg, rgba(22,26,38,0.98), rgba(15,18,27,0.96)); border: 1px solid rgba(245,197,66,0.18); box-shadow: 0 24px 60px rgba(0,0,0,0.45); display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 14px;">
                                <div style="grid-column: 1 / -1; display: flex; flex-direction: column; gap: 6px; padding: 12px 14px; border-radius: 12px; border: 1px solid #2c3146; background: rgba(17,21,33,0.88);">
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">"Claims Editor"</div>
                                    <div style="color: #d9d4c3; font-size: 0.84rem;">
                                        {move || {
                                            if live_bootstrap_pending.get() {
                                                "Syncing live territory data in the background…".to_string()
                                            } else {
                                                "Live territory data ready.".to_string()
                                            }
                                        }}
                                    </div>
                                    {move || live_bootstrap_error.get().map(|message| {
                                        view! {
                                            <div style="padding: 8px 10px; border-radius: 8px; border: 1px solid rgba(190,72,72,0.4); background: rgba(190,72,72,0.12); color: #ffcfcf; font-size: 0.78rem;">
                                                {message}
                                            </div>
                                        }
                                    })}
                                </div>
                                <button
                                    style="padding: 18px; border-radius: 14px; border: 1px solid #36405d; background: #131928; color: #e6e3d9; cursor: pointer; text-align: left;"
                                    on:click=move |_| {
                                        let mut document = ClaimDocumentV1::blank();
                                        document.view = default_view_from(
                                            &viewport.get_untracked(),
                                            &active_owner.get_untracked(),
                                        );
                                        apply_document_to_session(
                                            active_owner,
                                            viewport,
                                            session,
                                            tab,
                                            status_message,
                                            error_message,
                                            document,
                                            None,
                                            false,
                                        );
                                    }
                                >
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542; margin-bottom: 8px;">"Blank"</div>
                                    <div>"Start with every territory neutral."</div>
                                </button>
                                <button
                                    style="padding: 18px; border-radius: 14px; border: 1px solid #36405d; background: #131928; color: #e6e3d9; cursor: pointer; text-align: left;"
                                    on:click=move |_| {
                                        let live_map = live_territories.get_untracked();
                                        if live_map.is_empty() {
                                            return;
                                        }
                                        let mut document = ClaimDocumentV1::frozen_live(
                                            None,
                                            live_seq.get_untracked(),
                                            current_live_owner_map(&live_map),
                                        );
                                        document.view = default_view_from(
                                            &viewport.get_untracked(),
                                            &active_owner.get_untracked(),
                                        );
                                        apply_document_to_session(
                                            active_owner,
                                            viewport,
                                            session,
                                            tab,
                                            status_message,
                                            error_message,
                                            document,
                                            None,
                                            false,
                                        );
                                    }
                                    disabled=move || live_bootstrap_pending.get()
                                >
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542; margin-bottom: 8px;">"Current Live Map"</div>
                                    <div>"Freeze the current live map as the starting state."</div>
                                </button>
                                <button
                                    style="padding: 18px; border-radius: 14px; border: 1px solid #36405d; background: #131928; color: #e6e3d9; cursor: pointer; text-align: left;"
                                    on:click=move |_| trigger_import_picker(import_input_ref.clone())
                                >
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542; margin-bottom: 8px;">"Import File"</div>
                                    <div>"Load a `.json` claim export."</div>
                                </button>
                                <button
                                    style="padding: 18px; border-radius: 14px; border: 1px solid #36405d; background: #131928; color: #e6e3d9; cursor: pointer; text-align: left;"
                                    on:click=move |_| {
                                        if let Some(draft) = read_local_draft() {
                                            apply_document_to_session(
                                                active_owner,
                                                viewport,
                                                session,
                                                tab,
                                                status_message,
                                                error_message,
                                                draft.document,
                                                draft.source_snapshot_id,
                                                draft.follow_live,
                                            );
                                            active_owner.set(draft.active_owner.clone());
                                            session.update(|state| {
                                                if let Some(state) = state.as_mut() {
                                                    state.selection = draft.selection.clone();
                                                }
                                            });
                                        }
                                    }
                                    disabled=move || !has_local_draft.get()
                                >
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542; margin-bottom: 8px;">"Continue Draft"</div>
                                    <div>"Resume the latest local working copy."</div>
                                </button>
                                <div style="padding: 18px; border-radius: 14px; border: 1px solid #36405d; background: #131928; color: #e6e3d9; display: flex; flex-direction: column; gap: 8px;">
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">"Open Saved Snapshot"</div>
                                    <input
                                        prop:value=move || saved_snapshot_id_input.get()
                                        placeholder="Snapshot id"
                                        style="padding: 8px 10px; border-radius: 8px; border: 1px solid #3a415c; background: #111521; color: #e6e3d9;"
                                        on:input=move |event| saved_snapshot_id_input.set(event_target_value(&event))
                                    />
                                    <button
                                        style="padding: 10px 12px; border-radius: 8px; border: 1px solid #3a415c; background: #171b28; color: #e6e3d9; cursor: pointer;"
                                        on:click=move |_| {
                                            let id = saved_snapshot_id_input.get_untracked();
                                            if id.trim().is_empty() {
                                                return;
                                            }
                                            if let Some(window) = web_sys::window() {
                                                let _ = window.location().set_href(&format!("/claims/s/{}", id.trim()));
                                            }
                                        }
                                    >
                                        "Open"
                                    </button>
                                </div>
                                <div style="grid-column: 1 / -1; display: flex; flex-direction: column; gap: 8px;">
                                    <div style="font-family: 'Silkscreen', monospace; color: #f5c542;">"Local Presets"</div>
                                    <div style="display: flex; flex-wrap: wrap; gap: 8px;">
                                        {move || local_presets.get().into_iter().map(|preset| {
                                            let label = preset.name.clone();
                                            let document = preset.document.clone();
                                            view! {
                                                <button
                                                    style="padding: 10px 12px; border-radius: 10px; border: 1px solid #36405d; background: #131928; color: #e6e3d9; cursor: pointer;"
                                                    on:click=move |_| {
                                                        apply_document_to_session(
                                                            active_owner,
                                                            viewport,
                                                            session,
                                                            tab,
                                                            status_message,
                                                            error_message,
                                                            document.clone(),
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
                            </div>
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
}
