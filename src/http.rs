use crate::{
    adapters::official,
    config::Config,
    engine::Engine,
    store::{MemoryDraft, MemoryKind, Settings, Store},
    types::now,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub engine: Engine,
    pub store: Store,
    pub cfg: Arc<Config>,
    pub admin_hash: Arc<String>,
}
#[derive(Deserialize)]
pub struct Limit {
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    100
}
#[derive(Deserialize)]
pub struct MemoryQuery {
    #[serde(default)]
    session: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}
#[derive(Deserialize)]
pub struct MemoryInput {
    session: String,
    #[serde(default = "default_memory_kind")]
    kind: MemoryKind,
    subject: String,
    text: String,
    #[serde(default = "default_importance")]
    importance: u8,
}
fn default_memory_kind() -> MemoryKind {
    MemoryKind::Fact
}
fn default_importance() -> u8 {
    2
}
#[derive(Deserialize)]
pub struct PersonaInput {
    name: String,
    content: String,
}
#[derive(Deserialize)]
pub struct SessionPersona {
    persona: Option<String>,
}

pub fn router(engine: Engine, cfg: Arc<Config>, token: &str) -> Router {
    let store = engine.store().clone();
    let state = AppState {
        engine,
        store,
        cfg,
        admin_hash: Arc::new(hex::encode(sha2::Sha256::digest(token.as_bytes()))),
    };
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/official/webhook", post(official_webhook))
        .route("/api/status", get(status))
        .route("/api/pet/state", get(pet_state))
        .route("/api/sessions", get(sessions))
        .route("/api/proactive/sessions", get(proactive_sessions))
        .route("/api/sessions/{key}", get(session).delete(delete_session))
        .route("/api/sessions/{key}/interrupt", post(interrupt_session))
        .route("/api/sessions/{key}/interject", post(interject))
        .route("/api/traces", get(traces))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/personas", get(personas).post(put_persona))
        .route(
            "/api/sessions/{key}/persona",
            get(get_persona).put(set_persona),
        )
        .route("/api/memories", get(memories).post(add_memory))
        .route("/api/memories/import", post(import_memories))
        .route(
            "/api/memories/{id}",
            put(update_memory).delete(delete_memory),
        )
        .route("/api/stickers", get(stickers))
        .route(
            "/api/stickers/{id}",
            put(update_sticker).delete(delete_sticker),
        )
        .route("/api/maintenance", post(maintenance))
        .route("/api/backup", get(backup))
        .route("/api/restore", post(restore))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .with_state(state)
}
async fn official_webhook(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    official::handle_webhook(&s.cfg, &s.engine, headers, body).await
}
async fn authenticate(State(s): State<AppState>, req: Request<Body>, next: Next) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/"
        || req.uri().path() == "/health"
        || req.uri().path() == "/official/webhook"
            && s.cfg.official.enabled
            && s.cfg.official.transport == "webhook"
    {
        return next.run(req).await;
    }
    let got = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.strip_prefix("Bearer "))
        .unwrap_or("");
    let hash = hex::encode(sha2::Sha256::digest(got.as_bytes()));
    if constant_eq(&hash, &s.admin_hash) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        )
            .into_response()
    }
}
fn constant_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
async fn index() -> Html<&'static str> {
    Html(include_str!("panel.html"))
}
async fn health() -> Json<Value> {
    Json(json!({"ok":true,"time":now()}))
}
async fn status(State(s): State<AppState>) -> Json<Value> {
    let st = s.engine.settings_handle().lock().await.clone();
    Json(
        json!({"ok":true,"enabled":st.enabled,"dry_run":st.dry_run,"official_enabled":s.cfg.official.enabled,"onebot_enabled":s.cfg.onebot.enabled,"voice_enabled":s.cfg.voice.enabled,"queue_capacity":s.cfg.server.queue_capacity,"workers":s.cfg.server.workers}),
    )
}
async fn pet_state(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let settings = s.engine.settings_handle().lock().await.clone();
    let mut mood_sum = 0.0_f32;
    let mut count = 0_u32;
    let mut latest_user = 0_i64;
    let mut latest_assistant = 0_i64;
    for session in s.store.sessions(500)? {
        if !validate_proactive_session(&s.store, &session.key, &s.cfg.chat.owners)? {
            continue;
        }
        mood_sum += s.store.mood(&session.key);
        count += 1;
        if let Ok((user, assistant)) = s.store.activity(&session.key) {
            latest_user = latest_user.max(user.unwrap_or(0));
            latest_assistant = latest_assistant.max(assistant.unwrap_or(0));
        }
    }
    let mood = if count == 0 {
        0.0
    } else {
        mood_sum / count as f32
    };
    let local = (chrono::Utc::now().timestamp() + i64::from(settings.utc_offset_hours) * 3600)
        .rem_euclid(86400) as u32
        / 3600;
    let sleeping = !is_awake(local, settings.awake_start, settings.awake_end);
    let thinking = latest_user > latest_assistant && now() - latest_user < 180;
    let activity = if sleeping {
        "sleeping"
    } else if thinking {
        "thinking"
    } else {
        "idle"
    };
    let mood_label = if mood > 0.8 {
        "开心"
    } else if mood < -0.8 {
        "低落"
    } else {
        "平静"
    };
    Ok(Json(
        json!({"name":"大肥鱼","activity":activity,"mood":mood,"mood_label":mood_label,"owner_sessions":count,"chat_enabled":settings.enabled,"proactive_enabled":settings.proactive}),
    ))
}
fn is_awake(hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        start <= hour && hour < end
    } else {
        hour >= start || hour < end
    }
}
async fn sessions(
    State(s): State<AppState>,
    Query(q): Query<Limit>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(s.store.sessions(q.limit)?)))
}
async fn proactive_sessions(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let sessions = s.store.sessions(500)?;
    let allowed: Vec<_> = sessions
        .into_iter()
        .filter(|session| {
            if session.scene == "private" {
                s.cfg
                    .chat
                    .owners
                    .iter()
                    .any(|owner| owner == &session.target)
            } else if session.scene == "guild_dm" {
                s.store
                    .last_user_sender(&session.key)
                    .ok()
                    .flatten()
                    .is_some_and(|sender| s.cfg.chat.owners.iter().any(|owner| owner == &sender))
            } else {
                false
            }
        })
        .collect();
    Ok(Json(json!(allowed)))
}
fn validate_proactive_session(
    store: &Store,
    key: &str,
    owners: &[String],
) -> Result<bool, ApiError> {
    let (channel, scene, target) =
        Store::session_parts(key).map_err(|_| ApiError::bad("invalid proactive session"))?;
    if !["onebot", "official"].contains(&channel.as_str())
        || !["private", "guild_dm"].contains(&scene.as_str())
    {
        return Ok(false);
    }
    let identity = if scene == "private" {
        target
    } else {
        store
            .last_user_sender(key)?
            .ok_or_else(|| ApiError::bad("proactive session has no owner messages"))?
    };
    Ok(owners.iter().any(|owner| owner == &identity))
}
async fn session(
    State(s): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<Limit>,
) -> Result<Json<Value>, ApiError> {
    if key.len() > 512 {
        return Err(ApiError::bad("invalid session key"));
    }
    Ok(Json(json!(s.store.messages(&key, q.limit)?)))
}
async fn delete_session(
    State(s): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if key.len() > 512 {
        return Err(ApiError::bad("invalid session key"));
    }
    s.store.forget_session(&key)?;
    Ok(Json(json!({"ok":true})))
}
async fn interrupt_session(
    State(s): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if key.len() > 512 {
        return Err(ApiError::bad("invalid session key"));
    }
    Store::session_parts(&key).map_err(|_| ApiError::bad("invalid session key"))?;
    let interrupted = s.engine.interrupt(&key).await;
    Ok(Json(json!({"interrupted":interrupted})))
}
#[derive(Deserialize)]
struct Interjection {
    text: String,
}
async fn interject(
    State(s): State<AppState>,
    Path(key): Path<String>,
    Json(v): Json<Interjection>,
) -> Result<Json<Value>, ApiError> {
    if v.text.trim().is_empty() || v.text.len() > 4000 {
        return Err(ApiError::bad("message must be 1..4000 characters"));
    }
    s.engine.interject(&key, &v.text).await?;
    Ok(Json(json!({"queued":true})))
}
async fn traces(
    State(s): State<AppState>,
    Query(q): Query<Limit>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(s.store.traces(q.limit)?)))
}
async fn get_settings(State(s): State<AppState>) -> Json<Value> {
    let x = s.engine.settings_handle().lock().await.clone();
    Json(
        json!({"enabled":x.enabled,"dry_run":x.dry_run,"persona":x.persona,"group_mode":x.group_mode,"voice_replies":x.voice_replies,"proactive":x.proactive,"context_messages":x.context_messages,"group_keywords":x.group_keywords,"cooldown_secs":x.cooldown_secs,"memory_limit":x.memory_limit,"memory_recall":x.memory_recall,"auto_memory":x.auto_memory,"awake_start":x.awake_start,"awake_end":x.awake_end,"utc_offset_hours":x.utc_offset_hours,"proactive_interval_minutes":x.proactive_interval_minutes,"proactive_sessions":x.proactive_sessions,"proactive_goals":x.proactive_goals,"night_message":x.night_message,"night_message_hour":x.night_message_hour,"max_reply_chars":s.cfg.chat.max_reply_chars,"max_reply_parts":s.cfg.chat.max_reply_parts,"model_name":s.cfg.model.name,"vision":s.cfg.model.vision,"tts_model":s.cfg.voice.tts_model,"asr_model":s.cfg.voice.asr_model,"tts_voice":s.cfg.voice.tts_voice,"sticker_count":s.store.stickers(100).map(|x|x.len()).unwrap_or(0)}),
    )
}
async fn put_settings(
    State(s): State<AppState>,
    Json(v): Json<Settings>,
) -> Result<Json<Value>, ApiError> {
    if v.persona.len() > 50
        || !(1..=100).contains(&v.context_messages)
        || !["mention", "keyword", "all"].contains(&v.group_mode.as_str())
        || !(1..=100_000).contains(&v.memory_limit)
        || v.memory_recall > 20
        || v.awake_start > 23
        || !(1..=24).contains(&v.awake_end)
        || !(-12..=14).contains(&v.utc_offset_hours)
        || !(5..=1440).contains(&v.proactive_interval_minutes)
        || v.night_message_hour > 23
        || v.proactive_goals.is_empty()
        || v.proactive_goals.len() > 12
        || v.proactive_goals
            .iter()
            .any(|g| g.trim().is_empty() || g.len() > 200)
        || v.group_keywords.len() > 50
        || v.group_keywords.iter().any(|x| x.len() > 100)
        || v.proactive_sessions.len() > 500
        || v.proactive_sessions.iter().any(|x| x.len() > 512)
    {
        return Err(ApiError::bad("invalid settings"));
    }
    if !s
        .store
        .personas(&s.cfg.chat.personas)?
        .contains_key(&v.persona)
    {
        return Err(ApiError::bad("unknown persona"));
    }
    for key in &v.proactive_sessions {
        if !validate_proactive_session(&s.store, key, &s.cfg.chat.owners)? {
            return Err(ApiError::bad(
                "proactive sessions must be owner private chats",
            ));
        }
    }
    let handle = s.engine.settings_handle();
    let mut live = handle.lock().await;
    *live = v.clone();
    s.store.set_settings(&v)?;
    s.store.trace("panel", "settings", "chat settings updated");
    Ok(Json(json!({"ok":true,"settings":v})))
}
async fn personas(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(s.store.personas(&s.cfg.chat.personas)?)))
}
async fn get_persona(
    State(s): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if key.len() > 512 {
        return Err(ApiError::bad("invalid session key"));
    }
    Ok(Json(json!({"persona":s.store.session_persona(&key)})))
}
async fn put_persona(
    State(s): State<AppState>,
    Json(v): Json<PersonaInput>,
) -> Result<Json<Value>, ApiError> {
    if v.name.trim().is_empty()
        || v.name.len() > 50
        || v.content.trim().is_empty()
        || v.content.len() > 32000
    {
        return Err(ApiError::bad("invalid persona"));
    }
    s.store.set_persona(&v.name, &v.content)?;
    s.store.trace("panel", "persona", &v.name);
    Ok(Json(json!({"ok":true})))
}
async fn set_persona(
    State(s): State<AppState>,
    Path(key): Path<String>,
    Json(v): Json<SessionPersona>,
) -> Result<Json<Value>, ApiError> {
    if key.len() > 512 {
        return Err(ApiError::bad("invalid session key"));
    }
    if let Some(name) = &v.persona {
        if !s.store.personas(&s.cfg.chat.personas)?.contains_key(name) {
            return Err(ApiError::bad("unknown persona"));
        }
    }
    s.store.set_session_persona(&key, v.persona.as_deref())?;
    Ok(Json(json!({"ok":true})))
}
async fn memories(
    State(s): State<AppState>,
    Query(q): Query<MemoryQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(
        s.store.memories(&q.session, &q.q, q.limit, q.offset)?
    )))
}
async fn add_memory(
    State(s): State<AppState>,
    Json(v): Json<MemoryInput>,
) -> Result<Json<Value>, ApiError> {
    if v.session.len() > 512 || !valid_memory(&v) {
        return Err(ApiError::bad("invalid memory"));
    }
    s.store.add_typed_memory(
        &v.session,
        v.kind,
        &v.subject,
        &v.text,
        v.importance,
        s.cfg.chat.memory_limit,
    )?;
    Ok(Json(json!({"ok":true})))
}
async fn update_memory(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(v): Json<MemoryInput>,
) -> Result<Json<Value>, ApiError> {
    if !valid_memory(&v) {
        return Err(ApiError::bad("invalid memory"));
    }
    let draft = MemoryDraft {
        session: v.session,
        kind: v.kind,
        subject: v.subject,
        text: v.text,
        importance: v.importance,
    };
    Ok(Json(json!({"updated":s.store.update_memory(id,&draft)?})))
}
async fn import_memories(
    State(s): State<AppState>,
    Json(values): Json<Vec<MemoryInput>>,
) -> Result<Json<Value>, ApiError> {
    if values.len() > 500 || values.iter().any(|v| !valid_memory(v)) {
        return Err(ApiError::bad(
            "archive must contain at most 500 valid memories",
        ));
    }
    let drafts: Vec<_> = values
        .into_iter()
        .map(|v| MemoryDraft {
            session: v.session,
            kind: v.kind,
            subject: v.subject,
            text: v.text,
            importance: v.importance,
        })
        .collect();
    let count = drafts.len();
    s.store.add_memories(&drafts, s.cfg.chat.memory_limit)?;
    Ok(Json(json!({"imported":count})))
}
fn valid_memory(v: &MemoryInput) -> bool {
    !v.session.is_empty()
        && v.session.len() <= 512
        && !v.text.trim().is_empty()
        && v.text.len() <= 2000
        && v.subject.len() <= 100
        && v.importance <= 5
}
async fn delete_memory(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({"deleted":s.store.delete_memory(id)?})))
}
async fn stickers(
    State(s): State<AppState>,
    Query(q): Query<Limit>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(s.store.stickers(q.limit)?)))
}
#[derive(Deserialize)]
struct StickerUpdate {
    name: String,
    tags: String,
}
async fn update_sticker(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(v): Json<StickerUpdate>,
) -> Result<Json<Value>, ApiError> {
    if v.name.len() > 100 || v.tags.len() > 300 {
        return Err(ApiError::bad("name/tags too long"));
    }
    Ok(Json(
        json!({"updated":s.store.update_sticker(id,&v.name,&v.tags)?}),
    ))
}
async fn delete_sticker(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({"deleted":s.store.delete_sticker(id)?})))
}
async fn maintenance(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    s.store
        .janitor(s.cfg.server.retention_days, s.cfg.chat.memory_limit)?;
    Ok(Json(json!({"ok":true})))
}
async fn backup(State(s): State<AppState>) -> Result<Response, ApiError> {
    let bytes = s.store.backup()?;
    let mut r = Response::new(Body::from(bytes));
    *r.status_mut() = StatusCode::OK;
    r.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.sqlite3"),
    );
    r.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=aurabot-backup.sqlite"),
    );
    Ok(r)
}
async fn restore(State(s): State<AppState>, body: Bytes) -> Result<Json<Value>, ApiError> {
    if body.len() > 256 * 1024 * 1024 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "backup too large",
        ));
    }
    let tmp = PathBuf::from(&s.cfg.server.data_dir)
        .join(format!("restore-{}.sqlite", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &body)?;
    let result = (|| -> anyhow::Result<()> {
        let candidate = Store::open(&tmp)?;
        candidate.sessions(1)?;
        s.store.restore(&tmp)?;
        Ok(())
    })();
    std::fs::remove_file(&tmp)?;
    result?;
    s.store.trace("panel", "restore", "database restored");
    Ok(Json(json!({"ok":true})))
}
#[derive(Debug)]
pub struct ApiError(StatusCode, String);
impl ApiError {
    fn bad(s: &str) -> Self {
        Self(StatusCode::BAD_REQUEST, s.into())
    }
    fn new(c: StatusCode, s: &str) -> Self {
        Self(c, s.into())
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error=%e,"management API failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "request failed".into())
    }
}
impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}
impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
use sha2::Digest;
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tower::ServiceExt;

    #[test]
    fn token_hash_comparison_is_exact() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
    }

    #[tokio::test]
    async fn management_routes_require_a_bearer_token() {
        let cfg = Arc::new(Config::default());
        let store = Store::open(Path::new(":memory:")).unwrap();
        let engine = Engine::new(cfg.clone(), store).unwrap();
        let app = router(engine, cfg, "a sufficiently long test token value");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/settings")
                    .header(
                        axum::http::header::AUTHORIZATION,
                        "Bearer a sufficiently long test token value",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let settings: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(settings["group_mode"], "mention");
    }

    #[tokio::test]
    async fn interrupt_route_requires_auth_and_reports_when_idle() {
        let cfg = Arc::new(Config::default());
        let store = Store::open(Path::new(":memory:")).unwrap();
        let engine = Engine::new(cfg.clone(), store).unwrap();
        let app = router(engine, cfg, "a sufficiently long test token value");
        let uri = "/api/sessions/%5B%22onebot%22%2C%22private%22%2C%22user-1%22%5D/interrupt";
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(
                        axum::http::header::AUTHORIZATION,
                        "Bearer a sufficiently long test token value",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["interrupted"], false);
    }

    #[tokio::test]
    async fn pet_state_is_authenticated_and_does_not_include_chat_text() {
        let cfg = Arc::new(Config::default());
        let store = Store::open(Path::new(":memory:")).unwrap();
        let engine = Engine::new(cfg.clone(), store).unwrap();
        let app = router(engine, cfg, "a sufficiently long test token value");
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/pet/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/pet/state")
                    .header(
                        axum::http::header::AUTHORIZATION,
                        "Bearer a sufficiently long test token value",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let state: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(state.get("activity").is_some());
        assert!(state.get("messages").is_none());
        assert!(state.get("text").is_none());
    }

    #[tokio::test]
    async fn memory_api_searches_edits_and_imports_categories() {
        let cfg = Arc::new(Config::default());
        let store = Store::open(Path::new(":memory:")).unwrap();
        let engine = Engine::new(cfg.clone(), store).unwrap();
        let app = router(engine, cfg, "a sufficiently long test token value");
        let auth = "Bearer a sufficiently long test token value";
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/memories").header(axum::http::header::AUTHORIZATION,auth).header(axum::http::header::CONTENT_TYPE,"application/json").body(Body::from(r#"{"session":"*","kind":"profile","subject":"food","text":"likes tea","importance":3}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/memories?session=*&q=tea")
                    .header(axum::http::header::AUTHORIZATION, auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let found: Value = serde_json::from_slice(&bytes).unwrap();
        let id = found[0]["id"].as_i64().unwrap();
        assert_eq!(found[0]["kind"], "profile");
        let response = app.clone().oneshot(Request::builder().method("PUT").uri(format!("/api/memories/{id}")).header(axum::http::header::AUTHORIZATION,auth).header(axum::http::header::CONTENT_TYPE,"application/json").body(Body::from(r#"{"session":"*","kind":"style","subject":"answers","text":"prefers brief replies","importance":4}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app.oneshot(Request::builder().method("POST").uri("/api/memories/import").header(axum::http::header::AUTHORIZATION,auth).header(axum::http::header::CONTENT_TYPE,"application/json").body(Body::from(r#"[{"session":"*","kind":"experience","subject":"conversation","text":"enjoys discussing music","importance":2}]"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["imported"],
            1
        );
    }

    #[test]
    fn proactive_allowlist_rejects_groups_and_non_owner_chats() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let owners = vec!["owner".to_owned()];
        let group = serde_json::to_string(&("onebot", "group", "room")).unwrap();
        let stranger = serde_json::to_string(&("onebot", "private", "stranger")).unwrap();
        let owner_dm = serde_json::to_string(&("onebot", "private", "owner")).unwrap();
        assert!(!validate_proactive_session(&store, &group, &owners).unwrap());
        assert!(!validate_proactive_session(&store, &stranger, &owners).unwrap());
        assert!(validate_proactive_session(&store, &owner_dm, &owners).unwrap());
    }
}
