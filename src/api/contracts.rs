use crate::api::routes::{
    BrowserSessionRequest, BrowserSessionResponse, HistoryStatsQuery, LibraryBrowseQueryParams,
    LibraryFoldersResponse, LibrarySearchQuery, PairingRevocationResponse, PairingStartResponse,
    ProfilesResponse, QobuzSearchQuery, QobuzStatusResponse, RecentAlbumsQuery, RecentHistoryQuery,
    RecentPlaylistsQuery, RecentSearchesResponse, RemoteAccessSettingsDto,
    RemoteAccessSettingsResponse, RemoteAccessSettingsUpdateRequest, RemoteLinkCodeResponse,
    RemoteSessionMetadataDto, RemoteSessionRequest, RemoteSessionResponse,
    RemoteSessionRevocationResponse, RemoteSessionsResponse, ZoneCalibrationResponse,
};
use crate::app::identity;
use crate::app::server_remote::RemoteAccessStatus;
use crate::library::{
    AlbumSummary, ArtistSummary, FavoriteAlbumSummary, LibraryBrowsePage, LibraryScanProgress,
    LibrarySearchResponse, LibrarySummary, ListeningHistoryStats, PlaybackHistoryEntry,
    PlaylistSummary, RecentAlbumSummary, RecentPlaylistSummary, TrackSummary, ZoneSettings,
};
use crate::playback::queue::NowPlayingQueueResponse;
use crate::playback::status::StatusResponse;
use crate::protocol::{AgentBufferState, SourceRef, SyncSignalPath, ZoneProfile};
use crate::services::qobuz::{
    QobuzAlbum, QobuzAlbumDetail, QobuzAlbumPageResponse, QobuzAlbumSearchResponse, QobuzArtist,
    QobuzArtistDetail, QobuzArtistImageResponse, QobuzArtistSearchResponse,
    QobuzFeaturedPlaylistsResponse, QobuzHomeResponse, QobuzPlaylist, QobuzPlaylistDetail,
    QobuzPlaylistTag, QobuzSearchResponse, QobuzStatus, QobuzTrack,
};
use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA_RELATIVE_PATH: &str = "ui/src/shared/generated/api-contract.schema.json";
const TYPES_RELATIVE_PATH: &str = "ui/src/shared/generated/api-types.ts";
const ENDPOINTS_RELATIVE_PATH: &str = "ui/src/shared/generated/api-endpoints.ts";
const TYPES_GENERATOR_RELATIVE_PATH: &str = "ui/scripts/generate-api-types.mjs";

#[derive(JsonSchema)]
// Schema generation needs a root type that exists only as a compile-time contract aggregate.
#[allow(dead_code)]
pub(crate) struct ApiContractTypes {
    pub playback_status: StatusResponse,
    pub websocket_playback_status: StatusResponse,
    pub now_playing_queue: NowPlayingQueueResponse,
    pub source_ref: SourceRef,
    pub sync_signal_path: SyncSignalPath,
    pub agent_buffer_state: AgentBufferState,
    pub zones: Vec<ZoneProfile>,
    pub zone_profile: ZoneProfile,
    pub zone_calibration: ZoneCalibrationResponse,
    pub zone_settings: ZoneSettings,
    pub library_summary: LibrarySummary,
    pub library_scan_progress: LibraryScanProgress,
    pub library_album: AlbumSummary,
    pub library_track: TrackSummary,
    pub library_artist: ArtistSummary,
    pub library_album_page: LibraryBrowsePage<AlbumSummary>,
    pub library_track_page: LibraryBrowsePage<TrackSummary>,
    pub library_artist_page: LibraryBrowsePage<ArtistSummary>,
    pub library_browse_query: LibraryBrowseQueryParams,
    pub library_search: LibrarySearchResponse,
    pub library_search_query: LibrarySearchQuery,
    pub library_folders: LibraryFoldersResponse,
    pub recent_albums_query: RecentAlbumsQuery,
    pub favorite_album: FavoriteAlbumSummary,
    pub recent_album: RecentAlbumSummary,
    pub playlist: PlaylistSummary,
    pub recent_playlist: RecentPlaylistSummary,
    pub recent_playlists_query: RecentPlaylistsQuery,
    pub playback_history_entry: PlaybackHistoryEntry,
    pub listening_history_stats: ListeningHistoryStats,
    pub recent_history_query: RecentHistoryQuery,
    pub history_stats_query: HistoryStatsQuery,
    pub profiles: ProfilesResponse,
    pub recent_searches: RecentSearchesResponse,
    pub qobuz_status_response: QobuzStatusResponse,
    pub qobuz_search_query: QobuzSearchQuery,
    pub qobuz_status: QobuzStatus,
    pub qobuz_track: QobuzTrack,
    pub qobuz_album: QobuzAlbum,
    pub qobuz_album_detail: QobuzAlbumDetail,
    pub qobuz_album_page: QobuzAlbumPageResponse,
    pub qobuz_album_search: QobuzAlbumSearchResponse,
    pub qobuz_artist: QobuzArtist,
    pub qobuz_artist_detail: QobuzArtistDetail,
    pub qobuz_artist_image: QobuzArtistImageResponse,
    pub qobuz_artist_search: QobuzArtistSearchResponse,
    pub qobuz_featured_playlists: QobuzFeaturedPlaylistsResponse,
    pub qobuz_home: QobuzHomeResponse,
    pub qobuz_playlist: QobuzPlaylist,
    pub qobuz_playlist_detail: QobuzPlaylistDetail,
    pub qobuz_playlist_tag: QobuzPlaylistTag,
    pub qobuz_search: QobuzSearchResponse,
    pub pairing_start: PairingStartResponse,
    pub browser_session_request: BrowserSessionRequest,
    pub browser_session: BrowserSessionResponse,
    pub pairing_revocation: PairingRevocationResponse,
    pub remote_access_settings: RemoteAccessSettingsDto,
    pub remote_access_status: RemoteAccessStatus,
    pub remote_access_settings_response: RemoteAccessSettingsResponse,
    pub remote_access_settings_update_request: RemoteAccessSettingsUpdateRequest,
    pub remote_link_code: RemoteLinkCodeResponse,
    pub remote_session_metadata: RemoteSessionMetadataDto,
    pub remote_sessions: RemoteSessionsResponse,
    pub remote_session_revocation: RemoteSessionRevocationResponse,
    pub remote_session_request: RemoteSessionRequest,
    pub remote_session: RemoteSessionResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct EndpointContract {
    pub method: &'static str,
    pub path: &'static str,
    pub response_schema: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_schema: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_schema: Option<&'static str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub path_params: &'static [&'static str],
    pub success_status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loose_reason: Option<&'static str>,
}

impl EndpointContract {
    fn get(path: &'static str, response_schema: &'static str) -> Self {
        Self {
            method: "GET",
            path,
            response_schema,
            request_schema: None,
            query_schema: None,
            path_params: &[],
            success_status: 200,
            loose_reason: None,
        }
    }

    fn post(path: &'static str, response_schema: &'static str) -> Self {
        Self {
            method: "POST",
            path,
            response_schema,
            request_schema: None,
            query_schema: None,
            path_params: &[],
            success_status: 200,
            loose_reason: None,
        }
    }

    fn query_schema(mut self, query_schema: &'static str) -> Self {
        self.query_schema = Some(query_schema);
        self
    }

    fn request_schema(mut self, request_schema: &'static str) -> Self {
        self.request_schema = Some(request_schema);
        self
    }

    fn path_params(mut self, path_params: &'static [&'static str]) -> Self {
        self.path_params = path_params;
        self
    }

    fn loose(mut self, reason: &'static str) -> Self {
        self.loose_reason = Some(reason);
        self
    }
}

#[derive(Debug)]
pub struct GeneratedContractPaths {
    pub schema_path: PathBuf,
    pub types_path: PathBuf,
    pub endpoints_path: PathBuf,
}

pub fn endpoint_contracts() -> Vec<EndpointContract> {
    vec![
        EndpointContract::get("/api/status", "StatusResponse"),
        EndpointContract::get("/api/ws", "StatusResponse"),
        EndpointContract::get("/api/zones", "ZoneProfile[]"),
        EndpointContract::post("/api/zones/:zone_id/calibrate", "ZoneCalibrationResponse")
            .path_params(&["zone_id"])
            .loose("UPnP calibration requires a discovered renderer, not the generic local-core sample."),
        EndpointContract::get("/api/zones/:zone_id/status", "StatusResponse")
            .path_params(&["zone_id"]),
        EndpointContract::get(
            "/api/zones/:zone_id/now-playing-queue",
            "NowPlayingQueueResponse",
        )
        .path_params(&["zone_id"]),
        EndpointContract::get("/api/library/summary", "LibrarySummary"),
        EndpointContract::get("/api/library/albums", "AlbumSummary[]"),
        EndpointContract::get("/api/library/tracks", "TrackSummary[]"),
        EndpointContract::get("/api/library/artists", "ArtistSummary[]"),
        EndpointContract::get(
            "/api/library/browse/albums",
            "LibraryBrowsePage_for_AlbumSummary",
        )
        .query_schema("LibraryBrowseQueryParams"),
        EndpointContract::get(
            "/api/library/browse/tracks",
            "LibraryBrowsePage_for_TrackSummary",
        )
        .query_schema("LibraryBrowseQueryParams"),
        EndpointContract::get(
            "/api/library/browse/artists",
            "LibraryBrowsePage_for_ArtistSummary",
        )
        .query_schema("LibraryBrowseQueryParams"),
        EndpointContract::get("/api/library/folders", "LibraryFoldersResponse"),
        EndpointContract::get("/api/library/search", "LibrarySearchResponse")
            .query_schema("LibrarySearchQuery"),
        EndpointContract::get("/api/library/recent-albums", "RecentAlbumSummary[]")
            .query_schema("RecentAlbumsQuery"),
        EndpointContract::get("/api/history/recent", "PlaybackHistoryEntry[]")
            .query_schema("RecentHistoryQuery"),
        EndpointContract::get("/api/history/stats", "ListeningHistoryStats")
            .query_schema("HistoryStatsQuery"),
        EndpointContract::get("/api/playlists", "PlaylistSummary[]"),
        EndpointContract::get("/api/playlists/recent", "RecentPlaylistSummary[]")
            .query_schema("RecentPlaylistsQuery"),
        EndpointContract::get("/api/profiles", "ProfilesResponse"),
        EndpointContract::get("/api/qobuz/status", "QobuzStatusResponse"),
        EndpointContract::get("/api/qobuz/search", "QobuzSearchResponse")
            .query_schema("QobuzSearchQuery"),
        EndpointContract::get("/api/qobuz/home", "QobuzHomeResponse"),
        EndpointContract::post("/api/pairing/start", "PairingStartResponse"),
        EndpointContract::post("/api/sessions/browser", "BrowserSessionResponse")
            .request_schema("BrowserSessionRequest"),
        EndpointContract::post("/api/agents/token", "PairingStartResponse"),
        EndpointContract::post("/api/pairing/revoke-current", "PairingRevocationResponse"),
        EndpointContract::post("/api/pairing/revoke-all", "PairingRevocationResponse"),
        EndpointContract::get("/api/remote/settings", "RemoteAccessSettingsResponse"),
        EndpointContract::get("/api/remote/status", "RemoteAccessStatus"),
        EndpointContract::post("/api/remote/settings", "RemoteAccessSettingsResponse")
            .request_schema("RemoteAccessSettingsUpdateRequest"),
        EndpointContract::post("/api/remote/link-code", "RemoteLinkCodeResponse"),
        EndpointContract::get("/api/remote/sessions", "RemoteSessionsResponse"),
        EndpointContract::post(
            "/api/remote/sessions/:id/revoke",
            "RemoteSessionRevocationResponse",
        )
        .path_params(&["id"]),
        EndpointContract::post("/api/remote/session", "RemoteSessionResponse")
            .request_schema("RemoteSessionRequest")
            .loose("Remote session exchange is registered only on the TLS remote router."),
        EndpointContract::get("/api/qobuz/raw-proxy/*", "JsonRecord").loose(
            "Qobuz proxy/debug endpoints intentionally preserve third-party-shaped payloads.",
        ),
    ]
}

pub fn api_contract_schema_value() -> Value {
    let mut value = serde_json::to_value(schema_for!(ApiContractTypes))
        .expect("api contract schema should serialize");
    if let Value::Object(root) = &mut value {
        root.insert(
            "$id".to_string(),
            Value::String(format!(
                "{}/api-contract.schema.json",
                identity::SCHEMA_BASE_URL
            )),
        );
        root.insert(
            "title".to_string(),
            Value::String(format!("{}ApiContract", identity::APP_DISPLAY_NAME)),
        );
        root.insert(
            identity::SCHEMA_ENDPOINTS_EXTENSION.to_string(),
            serde_json::to_value(endpoint_contracts())
                .expect("api contract endpoints should serialize"),
        );
    }
    value
}

pub fn api_contract_schema_string() -> String {
    let value = api_contract_schema_value();
    let mut text =
        serde_json::to_string_pretty(&value).expect("api contract schema should serialize");
    text.push('\n');
    text
}

pub fn api_contract_schema_hash(schema_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(schema_text.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn write_contract_schema(repo_root: &Path) -> io::Result<PathBuf> {
    let schema_path = repo_root.join(SCHEMA_RELATIVE_PATH);
    if let Some(parent) = schema_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&schema_path, api_contract_schema_string())?;
    Ok(schema_path)
}

pub fn generate_contract_artifacts(repo_root: &Path) -> io::Result<GeneratedContractPaths> {
    let schema_path = write_contract_schema(repo_root)?;
    let types_path = repo_root.join(TYPES_RELATIVE_PATH);
    let endpoints_path = repo_root.join(ENDPOINTS_RELATIVE_PATH);
    let status = Command::new("node")
        .arg(repo_root.join(TYPES_GENERATOR_RELATIVE_PATH))
        .arg(&schema_path)
        .arg(&types_path)
        .arg(&endpoints_path)
        .current_dir(repo_root)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "TypeScript contract generation failed with {status}"
        )));
    }
    Ok(GeneratedContractPaths {
        schema_path,
        types_path,
        endpoints_path,
    })
}

pub fn committed_schema_path(repo_root: &Path) -> PathBuf {
    repo_root.join(SCHEMA_RELATIVE_PATH)
}

pub fn committed_types_path(repo_root: &Path) -> PathBuf {
    repo_root.join(TYPES_RELATIVE_PATH)
}

pub fn committed_endpoints_path(repo_root: &Path) -> PathBuf {
    repo_root.join(ENDPOINTS_RELATIVE_PATH)
}

pub fn schema_for_definition(definition_name: &str) -> Option<Value> {
    schema_for_contract_schema(definition_name)
}

pub fn schema_for_response_schema(response_schema: &str) -> Option<Value> {
    schema_for_contract_schema(response_schema)
}

pub fn schema_for_contract_schema(response_schema: &str) -> Option<Value> {
    let value = api_contract_schema_value();
    let definitions = value.get("definitions")?.as_object()?;
    let response_schema = response_schema.trim();
    let mut root = Map::new();
    root.insert(
        "$schema".to_string(),
        Value::String("http://json-schema.org/draft-07/schema#".to_string()),
    );
    root.insert(
        "definitions".to_string(),
        Value::Object(definitions.clone()),
    );
    if let Some(item_schema) = response_schema.strip_suffix("[]") {
        if !definitions.contains_key(item_schema) {
            return None;
        }
        root.insert("type".to_string(), Value::String("array".to_string()));
        root.insert(
            "items".to_string(),
            serde_json::json!({ "$ref": format!("#/definitions/{item_schema}") }),
        );
    } else if response_schema == "JsonRecord" {
        root.insert("type".to_string(), Value::String("object".to_string()));
        root.insert("additionalProperties".to_string(), Value::Bool(true));
    } else {
        if !definitions.contains_key(response_schema) {
            return None;
        }
        root.insert(
            "allOf".to_string(),
            Value::Array(vec![serde_json::json!({
                "$ref": format!("#/definitions/{response_schema}")
            })]),
        );
    }
    Some(Value::Object(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::create_router;
    use crate::playback::test_support::app_state;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode, header::CONTENT_TYPE};
    use tower::ServiceExt;

    #[test]
    fn committed_schema_and_types_are_fresh() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let expected_schema = api_contract_schema_string();
        let committed_schema = fs::read_to_string(committed_schema_path(repo_root))
            .expect("committed api contract schema should exist");
        assert_eq!(
            committed_schema, expected_schema,
            "run `cargo run --release --bin generate_api_contracts`"
        );

        let committed_types = fs::read_to_string(committed_types_path(repo_root))
            .expect("committed api contract TypeScript should exist");
        let expected_hash = api_contract_schema_hash(&expected_schema);
        assert!(
            committed_types.contains(&format!("schema-sha256: {expected_hash}")),
            "generated TypeScript is stale; run `cargo run --release --bin generate_api_contracts`"
        );

        let committed_endpoints = fs::read_to_string(committed_endpoints_path(repo_root))
            .expect("committed api contract endpoint metadata should exist");
        assert!(
            committed_endpoints.contains(&format!("schema-sha256: {expected_hash}")),
            "generated TypeScript endpoint metadata is stale; run `cargo run --release --bin generate_api_contracts`"
        );
    }

    #[tokio::test]
    async fn contracted_routes_match_contract_schemas() {
        let state = app_state("contracts");
        let app = create_router().with_state(state.clone());

        for contract in endpoint_contracts() {
            if contract.loose_reason.is_some() || contract.path == "/api/ws" {
                continue;
            }
            if !cfg!(feature = "qobuz") && contract.path.starts_with("/api/qobuz") {
                continue;
            }
            let Some(path) = sample_path(contract.path) else {
                continue;
            };
            if let Some(query_schema) = contract.query_schema {
                let sample = sample_query(contract.path);
                assert_json_matches_schema(
                    &format!("{} {} query", contract.method, contract.path),
                    &sample,
                    schema_for_contract_schema(query_schema)
                        .unwrap_or_else(|| panic!("missing schema for {query_schema}")),
                );
            }
            let request_body = sample_request_body(&state, &contract);
            if let Some(request_schema) = contract.request_schema {
                let sample = request_body.clone().unwrap_or(Value::Null);
                assert_json_matches_schema(
                    &format!("{} {} request", contract.method, contract.path),
                    &sample,
                    schema_for_contract_schema(request_schema)
                        .unwrap_or_else(|| panic!("missing schema for {request_schema}")),
                );
            }
            let schema = schema_for_response_schema(contract.response_schema)
                .unwrap_or_else(|| panic!("missing schema for {}", contract.response_schema));
            assert_route_matches_schema(
                app.clone(),
                &state,
                &contract,
                &path,
                request_body,
                schema,
            )
            .await;
        }
    }

    #[test]
    fn non_loose_contracts_do_not_use_json_record() {
        for contract in endpoint_contracts() {
            if contract.loose_reason.is_none() {
                assert_ne!(
                    contract.response_schema, "JsonRecord",
                    "{} {} must use a named response schema or mark a loose_reason",
                    contract.method, contract.path
                );
                assert_ne!(
                    contract.request_schema,
                    Some("JsonRecord"),
                    "{} {} must use a named request schema or mark a loose_reason",
                    contract.method,
                    contract.path
                );
            }
        }
    }

    async fn assert_route_matches_schema(
        app: axum::Router,
        state: &crate::app::state::AppState,
        contract: &EndpointContract,
        path: &str,
        body: Option<Value>,
        schema: Value,
    ) {
        let method: Method = contract
            .method
            .parse()
            .expect("contract method should be a valid HTTP method");
        let uri = with_sample_query(path, contract);
        let mut request = Request::builder()
            .method(method)
            .uri(&uri)
            .header("host", "127.0.0.1:0");
        if contract.path == "/api/pairing/revoke-current" {
            let token = state
                .pairing()
                .create_control_session(None)
                .expect("control session")
                .token;
            request = request.header(crate::app::identity::AUTH_HEADER, token);
        }
        let body = if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let response = app
            .oneshot(request.body(body).expect("request should build"))
            .await
            .expect("route should respond");
        assert_eq!(
            response.status(),
            StatusCode::from_u16(contract.success_status).expect("valid success status"),
            "{} {} should return expected status",
            contract.method,
            uri
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let json: Value = serde_json::from_slice(&body).expect("response should be JSON");
        assert_json_matches_schema(
            &format!("{} {uri} response", contract.method),
            &json,
            schema,
        );
    }

    fn assert_json_matches_schema(label: &str, json: &Value, schema: Value) {
        let compiled = jsonschema::JSONSchema::compile(&schema).expect("schema should compile");
        if let Err(errors) = compiled.validate(json) {
            let messages = errors.map(|error| error.to_string()).collect::<Vec<_>>();
            panic!("{label} did not match contract schema: {messages:?}\njson: {json}");
        }
    }

    fn sample_path(path: &str) -> Option<String> {
        if path.contains('*') {
            return None;
        }
        Some(
            path.replace(":zone_id", crate::zones::LOCAL_ZONE_ID)
                .replace(":id", "sample-id"),
        )
    }

    fn with_sample_query(path: &str, contract: &EndpointContract) -> String {
        let query = sample_query(contract.path);
        let Some(params) = query.as_object() else {
            return path.to_string();
        };
        let pairs = params
            .iter()
            .filter_map(|(key, value)| match value {
                Value::Null => None,
                Value::Bool(value) => Some(format!("{key}={value}")),
                Value::Number(value) => Some(format!("{key}={value}")),
                Value::String(value) if value.is_empty() => None,
                Value::String(value) => Some(format!("{key}={}", urlencoding::encode(value))),
                _ => None,
            })
            .collect::<Vec<_>>();
        if pairs.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{}", pairs.join("&"))
        }
    }

    fn sample_query(path: &str) -> Value {
        match path {
            "/api/library/browse/albums"
            | "/api/library/browse/tracks"
            | "/api/library/browse/artists" => serde_json::json!({
                "limit": 12,
                "offset": 0,
                "include_facets": true
            }),
            "/api/library/search" | "/api/qobuz/search" => serde_json::json!({ "q": "" }),
            "/api/library/recent-albums" => serde_json::json!({ "limit": 12 }),
            "/api/history/recent" => serde_json::json!({
                "limit": 12,
                "exclude_radio": false
            }),
            "/api/history/stats" => serde_json::json!({ "range": "4w" }),
            "/api/playlists/recent" => serde_json::json!({ "limit": 12 }),
            _ => Value::Object(Map::new()),
        }
    }

    fn sample_request_body(
        state: &crate::app::state::AppState,
        contract: &EndpointContract,
    ) -> Option<Value> {
        match (contract.method, contract.path) {
            ("POST", "/api/sessions/browser") => {
                let token = state.pairing().create_token().expect("pairing token").token;
                Some(serde_json::json!({ "pairing_token": token }))
            }
            ("POST", "/api/remote/settings") => Some(serde_json::json!({
                "enabled": false,
                "port": 9443,
                "external_host": "home.example.test",
                "custom_cert_path": "",
                "custom_key_path": ""
            })),
            _ => None,
        }
    }
}
