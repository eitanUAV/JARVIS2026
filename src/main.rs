// JARVIS2026 — Property listing & media upload API
// Actix-web + PostgreSQL. See README.md for configuration and deployment.

use actix_cors::Cors;
use actix_files as fs;
use actix_multipart::{Field, Multipart};
use actix_web::{
    get, http::header, middleware, post, web, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};
use uuid::Uuid;

// ============================================================================
// CONFIGURATION
// ============================================================================

/// Tokens awarded for the first upload of a previously unseen file.
const ORIGINAL_UPLOAD_TOKENS: i64 = 100;
/// Default per-file upload ceiling (25 MiB). Override with MAX_FILE_MB.
const DEFAULT_MAX_FILE_MB: u64 = 25;
/// Maximum number of files accepted in a single upload request.
const MAX_FILES_PER_UPLOAD: usize = 20;
/// Cap on any single multipart text field, to bound memory on junk input.
const MAX_TEXT_FIELD_BYTES: usize = 64 * 1024;
/// Default page size for property listings.
const DEFAULT_PAGE_LIMIT: i64 = 50;
/// Hard ceiling on page size, regardless of what the caller asks for.
const MAX_PAGE_LIMIT: i64 = 200;

struct Config {
    upload_dir: PathBuf,
    max_file_bytes: u64,
    api_key: Option<String>,
    allowed_origins: Vec<String>,
}

impl Config {
    fn from_env() -> Self {
        let max_file_mb = std::env::var("MAX_FILE_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_FILE_MB);

        let api_key = std::env::var("API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());

        let allowed_origins = parse_allowed_origins(
            &std::env::var("ALLOWED_ORIGINS").unwrap_or_else(|_| String::new()),
        );

        Config {
            upload_dir: PathBuf::from(
                std::env::var("UPLOAD_DIR").unwrap_or_else(|_| "uploads".to_string()),
            ),
            max_file_bytes: max_file_mb * 1024 * 1024,
            api_key,
            allowed_origins,
        }
    }
}

struct AppState {
    db: PgPool,
    config: Config,
}

// ============================================================================
// PURE HELPERS (unit-tested below)
// ============================================================================

/// Media types we are willing to store and serve back from our own origin.
/// Deliberately excludes anything the browser will execute (.html, .svg, .js).
const ALLOWED_EXTENSIONS: &[(&str, &str)] = &[
    ("jpg", "image"),
    ("jpeg", "image"),
    ("png", "image"),
    ("webp", "image"),
    ("gif", "image"),
    ("avif", "image"),
    ("mp4", "video"),
    ("mov", "video"),
    ("m4v", "video"),
    ("webm", "video"),
];

/// Extracts a safe, allowlisted extension from a client-supplied filename.
///
/// The returned value is a `&'static str` borrowed from `ALLOWED_EXTENSIONS`,
/// never from caller input, so it cannot carry path separators or traversal
/// sequences into a filesystem path.
fn safe_extension(filename: &str) -> Option<(&'static str, &'static str)> {
    // Strip any directory component a client may have sent (`../`, `C:\`, ...).
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .trim();
    let ext = base.rsplit_once('.')?.1.to_ascii_lowercase();
    ALLOWED_EXTENSIONS
        .iter()
        .find(|(allowed, _)| *allowed == ext)
        .map(|(e, t)| (*e, *t))
}

/// Escapes LIKE metacharacters so a user's search text is matched literally.
/// Pairs with `ESCAPE '\'` in the SQL.
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn parse_allowed_origins(raw: &str) -> Vec<String> {
    let configured: Vec<String> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if configured.is_empty() {
        vec![
            "http://localhost:8080".to_string(),
            "http://127.0.0.1:8080".to_string(),
        ]
    } else {
        configured
    }
}

/// Decides whether a browser Origin may call the API.
///
/// Requests are allowed when the Origin is explicitly configured, or when it is
/// the server's own origin. The same-origin case matters because the front-end
/// is served by this binary: without it, the bundled UI is refused by CORS on
/// any host that is not hardcoded into ALLOWED_ORIGINS, which is every fresh
/// deployment.
fn is_origin_allowed(origin: &str, host: Option<&str>, allowlist: &[String]) -> bool {
    if allowlist.iter().any(|allowed| allowed == origin) {
        return true;
    }

    match host {
        // Both schemes are accepted because TLS is usually terminated by a
        // proxy, leaving the app itself speaking plain HTTP behind an
        // https:// public origin.
        Some(host) => origin == format!("http://{host}") || origin == format!("https://{host}"),
        None => false,
    }
}

/// Summarises an upload for the user. Reporting "already in the system" when
/// files were actually rejected hides real failures behind a success message.
fn build_upload_message(tokens: i64, duplicates: usize, rejected: usize) -> String {
    let mut parts = Vec::new();

    if tokens > 0 {
        parts.push(format!("You earned {} tokens.", tokens));
    }
    if duplicates > 0 {
        parts.push(format!(
            "{} file(s) were already in the system and earned nothing.",
            duplicates
        ));
    }
    if rejected > 0 {
        parts.push(format!("{} file(s) were rejected.", rejected));
    }

    if parts.is_empty() {
        "Property published.".to_string()
    } else {
        format!("Property published. {}", parts.join(" "))
    }
}

/// Public URL path for a stored media object. Names are content hashes plus an
/// allowlisted extension, so they are always URL- and path-safe.
fn media_url(content_hash: &str, ext: &str) -> String {
    format!("/uploads/{}.{}", content_hash, ext)
}

fn stored_filename(content_hash: &str, ext: &str) -> String {
    format!("{}.{}", content_hash, ext)
}

fn clamp_limit(requested: Option<i64>) -> i64 {
    requested
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .min(MAX_PAGE_LIMIT)
}

fn clamp_offset(requested: Option<i64>) -> i64 {
    requested.filter(|v| *v >= 0).unwrap_or(0)
}

// ============================================================================
// DATA STRUCTURES
// ============================================================================

// NOTE: every column that the schema declares NULLable is `Option` here.
// A mismatch causes a row-decode failure at query time, which surfaces as a
// blanket 500 on the listing endpoints.
#[derive(Serialize, Deserialize, Clone, Debug, sqlx::FromRow)]
struct Property {
    id: Uuid,
    title: String,
    location: String,
    price: f64,
    description: Option<String>,
    image_thumb_webp: Option<String>,
    image_large_webp: Option<String>,
    bedrooms: Option<i32>,
    bathrooms: Option<i32>,
    area_sqm: Option<f64>,
    user_id: Option<Uuid>,
    content_hash: Option<String>,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
struct User {
    id: Uuid,
    username: String,
    wallet_address: Option<String>,
    token_balance: i64,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct MediaItem {
    property_id: Uuid,
    url: String,
    file_type: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct TokenTransaction {
    id: Uuid,
    amount: i64,
    transaction_type: String,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A property plus the media actually attached to it. The bare `properties`
/// row has no way to reference uploaded files, which is why uploads never
/// appeared in the gallery.
#[derive(Serialize)]
struct PropertyResponse {
    #[serde(flatten)]
    property: Property,
    media: Vec<MediaView>,
}

#[derive(Debug, Serialize, Clone)]
struct MediaView {
    url: String,
    file_type: String,
}

#[derive(Debug, Serialize)]
struct UploadResponse {
    success: bool,
    property_id: Uuid,
    media: Vec<MediaView>,
    tokens_earned: i64,
    originals: usize,
    duplicates: usize,
    rejected: Vec<RejectedFile>,
    message: String,
}

#[derive(Debug, Serialize)]
struct RejectedFile {
    filename: String,
    reason: String,
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
    wallet_address: Option<String>,
}

#[derive(Deserialize)]
struct SearchQuery {
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

// ============================================================================
// AUTH
// ============================================================================

/// Guards state-changing endpoints. When `API_KEY` is unset the API is open —
/// startup logs a prominent warning in that case.
fn require_api_key(req: &HttpRequest, state: &AppState) -> Result<(), HttpResponse> {
    let Some(expected) = state.config.api_key.as_deref() else {
        return Ok(());
    };

    let presented = req
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            req.headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .unwrap_or("");

    // Length-independent comparison to avoid leaking the key via timing.
    let matches = presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;

    if matches {
        Ok(())
    } else {
        Err(HttpResponse::Unauthorized()
            .json(serde_json::json!({"error": "Invalid or missing API key"})))
    }
}

// ============================================================================
// DATABASE
// ============================================================================

async fn init_db(pool: &PgPool) -> Result<(), sqlx::Error> {
    info!("Initializing database schema...");

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS users (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            username TEXT UNIQUE NOT NULL,
            wallet_address TEXT,
            token_balance BIGINT NOT NULL DEFAULT 0,
            created_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS properties (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            title TEXT NOT NULL,
            location TEXT NOT NULL,
            price DOUBLE PRECISION NOT NULL,
            description TEXT,
            image_thumb_webp TEXT,
            image_large_webp TEXT,
            bedrooms INTEGER,
            bathrooms INTEGER,
            area_sqm DOUBLE PRECISION,
            user_id UUID REFERENCES users(id),
            content_hash TEXT,
            created_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    // Global registry of distinct file contents. The PRIMARY KEY makes
    // "who uploaded this first" an atomic decision (INSERT ... ON CONFLICT),
    // instead of a check-then-act race that lets two uploaders both be paid.
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS media_content (
            content_hash TEXT PRIMARY KEY,
            first_user_id UUID REFERENCES users(id),
            file_name TEXT NOT NULL,
            file_type TEXT NOT NULL,
            file_size BIGINT NOT NULL,
            created_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS media_uploads (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            property_id UUID REFERENCES properties(id) ON DELETE CASCADE,
            user_id UUID REFERENCES users(id),
            file_path TEXT NOT NULL,
            file_type TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            file_size BIGINT NOT NULL,
            is_original BOOLEAN NOT NULL DEFAULT true,
            tokens_earned BIGINT NOT NULL DEFAULT 0,
            uploaded_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    // Migration for databases created by earlier versions: content_hash was
    // UNIQUE across the whole table, so a file already known to the system
    // could never be attached to a second property. Uniqueness belongs on
    // (property_id, content_hash) — one copy per listing, reusable elsewhere.
    sqlx::query(
        "ALTER TABLE media_uploads DROP CONSTRAINT IF EXISTS media_uploads_content_hash_key",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_media_property_hash
         ON media_uploads(property_id, content_hash)",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_media_content_hash ON media_uploads(content_hash)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_media_property ON media_uploads(property_id)")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_properties_created_at ON properties(created_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS token_transactions (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            user_id UUID REFERENCES users(id),
            media_id UUID REFERENCES media_uploads(id) ON DELETE SET NULL,
            amount BIGINT NOT NULL,
            transaction_type TEXT NOT NULL,
            created_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    info!("Database schema ready");
    Ok(())
}

/// Fetches media for a page of properties in one round trip.
async fn media_for_properties(
    pool: &PgPool,
    property_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<MediaView>>, sqlx::Error> {
    let mut map: std::collections::HashMap<Uuid, Vec<MediaView>> = std::collections::HashMap::new();
    if property_ids.is_empty() {
        return Ok(map);
    }

    let rows = sqlx::query_as::<_, MediaItem>(
        "SELECT property_id, file_path AS url, file_type
         FROM media_uploads
         WHERE property_id = ANY($1)
         ORDER BY uploaded_at ASC",
    )
    .bind(property_ids)
    .fetch_all(pool)
    .await?;

    for row in rows {
        map.entry(row.property_id).or_default().push(MediaView {
            url: row.url,
            file_type: row.file_type,
        });
    }
    Ok(map)
}

async fn attach_media(
    pool: &PgPool,
    properties: Vec<Property>,
) -> Result<Vec<PropertyResponse>, sqlx::Error> {
    let ids: Vec<Uuid> = properties.iter().map(|p| p.id).collect();
    let mut media = media_for_properties(pool, &ids).await?;
    Ok(properties
        .into_iter()
        .map(|property| {
            let media = media.remove(&property.id).unwrap_or_default();
            PropertyResponse { property, media }
        })
        .collect())
}

// ============================================================================
// API HANDLERS
// ============================================================================

#[get("/api/health")]
async fn health_check(state: web::Data<AppState>) -> impl Responder {
    // A health check that never touches the database will report "healthy"
    // while every real endpoint is failing.
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .is_ok();

    let body = serde_json::json!({
        "status": if db_ok { "healthy" } else { "degraded" },
        "service": "JARVIS2026",
        "version": env!("CARGO_PKG_VERSION"),
        "database": if db_ok { "up" } else { "down" },
    });

    if db_ok {
        HttpResponse::Ok().json(body)
    } else {
        HttpResponse::ServiceUnavailable().json(body)
    }
}

#[get("/api/properties")]
async fn get_properties(q: web::Query<ListQuery>, state: web::Data<AppState>) -> impl Responder {
    let limit = clamp_limit(q.limit);
    let offset = clamp_offset(q.offset);

    let properties = match sqlx::query_as::<_, Property>(
        "SELECT * FROM properties ORDER BY created_at DESC NULLS LAST, id LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to fetch properties: {}", e);
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to fetch properties"}));
        }
    };

    match attach_media(&state.db, properties).await {
        Ok(results) => HttpResponse::Ok().json(results),
        Err(e) => {
            error!("Failed to fetch property media: {}", e);
            HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to fetch properties"}))
        }
    }
}

#[get("/api/properties/{id}")]
async fn get_property(path: web::Path<Uuid>, state: web::Data<AppState>) -> impl Responder {
    let id = path.into_inner();

    match sqlx::query_as::<_, Property>("SELECT * FROM properties WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(property)) => match attach_media(&state.db, vec![property]).await {
            Ok(mut results) if !results.is_empty() => HttpResponse::Ok().json(results.remove(0)),
            Ok(_) => HttpResponse::NotFound().json(serde_json::json!({"error": "Not found"})),
            Err(e) => {
                error!("Failed to fetch media for {}: {}", id, e);
                HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": "Failed to fetch property"}))
            }
        },
        Ok(None) => {
            HttpResponse::NotFound().json(serde_json::json!({"error": "Property not found"}))
        }
        Err(e) => {
            error!("Failed to fetch property {}: {}", id, e);
            HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to fetch property"}))
        }
    }
}

#[post("/api/search")]
async fn search_properties(
    query: web::Json<SearchQuery>,
    state: web::Data<AppState>,
) -> impl Responder {
    // Escaped so that a query of "%" matches the literal character rather
    // than every row in the table.
    let search = format!("%{}%", escape_like(&query.query.to_lowercase()));
    let limit = clamp_limit(query.limit);
    let offset = clamp_offset(query.offset);

    let properties = match sqlx::query_as::<_, Property>(
        r#"SELECT * FROM properties
           WHERE LOWER(title) LIKE $1 ESCAPE '\'
              OR LOWER(location) LIKE $1 ESCAPE '\'
              OR LOWER(COALESCE(description, '')) LIKE $1 ESCAPE '\'
           ORDER BY created_at DESC NULLS LAST, id
           LIMIT $2 OFFSET $3"#,
    )
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Search failed: {}", e);
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Search failed"}));
        }
    };

    info!("Search returned {} results", properties.len());

    match attach_media(&state.db, properties).await {
        Ok(results) => HttpResponse::Ok().json(results),
        Err(e) => {
            error!("Failed to fetch media for search: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({"error": "Search failed"}))
        }
    }
}

#[post("/api/users")]
async fn create_user(
    req: HttpRequest,
    body: web::Json<CreateUserRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    if let Err(resp) = require_api_key(&req, &state) {
        return resp;
    }

    let username = body.username.trim();
    if username.is_empty() || username.chars().count() > 64 {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "username must be between 1 and 64 characters"}));
    }

    match sqlx::query_as::<_, User>(
        "INSERT INTO users (username, wallet_address) VALUES ($1, $2) RETURNING *",
    )
    .bind(username)
    .bind(&body.wallet_address)
    .fetch_one(&state.db)
    .await
    {
        Ok(user) => {
            info!("User created: {} ({})", user.username, user.id);
            HttpResponse::Ok().json(user)
        }
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            HttpResponse::Conflict().json(serde_json::json!({"error": "Username already taken"}))
        }
        Err(e) => {
            error!("Failed to create user: {}", e);
            HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to create user"}))
        }
    }
}

#[get("/api/users/{user_id}/balance")]
async fn get_user_balance(path: web::Path<Uuid>, state: web::Data<AppState>) -> impl Responder {
    let user_id = path.into_inner();

    match sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(user)) => HttpResponse::Ok().json(user),
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({"error": "User not found"})),
        Err(e) => {
            error!("Failed to fetch balance for {}: {}", user_id, e);
            HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to fetch user"}))
        }
    }
}

#[get("/api/users/{user_id}/transactions")]
async fn get_user_transactions(
    path: web::Path<Uuid>,
    q: web::Query<ListQuery>,
    state: web::Data<AppState>,
) -> impl Responder {
    let user_id = path.into_inner();
    let limit = clamp_limit(q.limit);
    let offset = clamp_offset(q.offset);

    match sqlx::query_as::<_, TokenTransaction>(
        "SELECT id, amount, transaction_type, created_at
         FROM token_transactions
         WHERE user_id = $1
         ORDER BY created_at DESC NULLS LAST, id
         LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(e) => {
            error!("Failed to fetch transactions for {}: {}", user_id, e);
            HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to fetch transactions"}))
        }
    }
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// A file that has been streamed to disk under a temporary name and hashed,
/// but not yet committed to its content-addressed location.
struct StagedFile {
    temp_path: PathBuf,
    content_hash: String,
    size: i64,
    ext: &'static str,
    file_type: &'static str,
}

enum StageError {
    TooLarge,
    Io(std::io::Error),
    Stream,
}

/// Streams one multipart field to a temporary file, hashing as it goes and
/// aborting past the size ceiling. The file contents are never held in memory
/// in full, so a large upload costs disk, not RAM.
async fn stage_file(
    field: &mut Field,
    upload_dir: &Path,
    max_bytes: u64,
    ext: &'static str,
    file_type: &'static str,
) -> Result<StagedFile, StageError> {
    let temp_path = upload_dir.join(format!(".incoming-{}", Uuid::new_v4()));
    let mut file = async_fs::File::create(&temp_path)
        .await
        .map_err(StageError::Io)?;

    let mut hasher = Sha256::new();
    let mut size: u64 = 0;

    while let Some(chunk) = field.next().await {
        let data = match chunk {
            Ok(d) => d,
            Err(_) => {
                let _ = async_fs::remove_file(&temp_path).await;
                return Err(StageError::Stream);
            }
        };

        size += data.len() as u64;
        if size > max_bytes {
            let _ = async_fs::remove_file(&temp_path).await;
            return Err(StageError::TooLarge);
        }

        hasher.update(&data);
        if let Err(e) = file.write_all(&data).await {
            let _ = async_fs::remove_file(&temp_path).await;
            return Err(StageError::Io(e));
        }
    }

    if let Err(e) = file.flush().await {
        let _ = async_fs::remove_file(&temp_path).await;
        return Err(StageError::Io(e));
    }
    drop(file);

    Ok(StagedFile {
        temp_path,
        content_hash: hex::encode(hasher.finalize()),
        size: size as i64,
        ext,
        file_type,
    })
}

/// Reads a multipart text field to a String, joining every chunk.
/// Reading only the first chunk silently truncates longer values.
async fn read_text_field(field: &mut Field) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = field.next().await {
        match chunk {
            Ok(data) => {
                if buf.len() + data.len() > MAX_TEXT_FIELD_BYTES {
                    let take = MAX_TEXT_FIELD_BYTES.saturating_sub(buf.len());
                    buf.extend_from_slice(&data[..take]);
                    break;
                }
                buf.extend_from_slice(&data);
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).trim().to_string()
}

#[post("/api/upload-property")]
async fn upload_property(
    req: HttpRequest,
    mut payload: Multipart,
    state: web::Data<AppState>,
) -> impl Responder {
    if let Err(resp) = require_api_key(&req, &state) {
        return resp;
    }

    let upload_dir = &state.config.upload_dir;
    let max_bytes = state.config.max_file_bytes;

    let mut user_id: Option<Uuid> = None;
    let mut title = String::new();
    let mut location = String::new();
    let mut price: f64 = 0.0;
    let mut description = String::new();
    let mut bedrooms: Option<i32> = None;
    let mut bathrooms: Option<i32> = None;
    let mut area_sqm: Option<f64> = None;
    let mut staged: Vec<StagedFile> = Vec::new();
    let mut rejected: Vec<RejectedFile> = Vec::new();

    // Any early return past this point must clean up already-staged temp files.
    macro_rules! bail {
        ($resp:expr) => {{
            for f in &staged {
                let _ = async_fs::remove_file(&f.temp_path).await;
            }
            return $resp;
        }};
    }

    while let Some(item) = payload.next().await {
        let mut field = match item {
            Ok(f) => f,
            Err(e) => {
                error!("Malformed multipart payload: {}", e);
                bail!(HttpResponse::BadRequest()
                    .json(serde_json::json!({"error": "Malformed multipart payload"})));
            }
        };

        let name = field.name().to_string();

        match name.as_str() {
            "user_id" => {
                let raw = read_text_field(&mut field).await;
                user_id = Uuid::parse_str(&raw).ok();
            }
            "title" => title = read_text_field(&mut field).await,
            "location" => location = read_text_field(&mut field).await,
            "price" => price = read_text_field(&mut field).await.parse().unwrap_or(0.0),
            "description" => description = read_text_field(&mut field).await,
            "bedrooms" => bedrooms = read_text_field(&mut field).await.parse().ok(),
            "bathrooms" => bathrooms = read_text_field(&mut field).await.parse().ok(),
            "area_sqm" => area_sqm = read_text_field(&mut field).await.parse().ok(),
            "files" => {
                let filename = field
                    .content_disposition()
                    .get_filename()
                    .unwrap_or("upload")
                    .to_string();

                if staged.len() >= MAX_FILES_PER_UPLOAD {
                    rejected.push(RejectedFile {
                        filename,
                        reason: format!("More than {} files per upload", MAX_FILES_PER_UPLOAD),
                    });
                    continue;
                }

                // The client-supplied name is used only to pick an allowlisted
                // extension. It never reaches the filesystem path — a name like
                // "../../static/app.js" would otherwise let an uploader
                // overwrite files served to every visitor.
                let Some((ext, file_type)) = safe_extension(&filename) else {
                    rejected.push(RejectedFile {
                        filename,
                        reason: "Unsupported file type".to_string(),
                    });
                    continue;
                };

                match stage_file(&mut field, upload_dir, max_bytes, ext, file_type).await {
                    Ok(f) => staged.push(f),
                    Err(StageError::TooLarge) => rejected.push(RejectedFile {
                        filename,
                        reason: format!("Exceeds {} MiB limit", max_bytes / (1024 * 1024)),
                    }),
                    Err(StageError::Io(e)) => {
                        error!("Failed to write upload to disk: {}", e);
                        bail!(HttpResponse::InternalServerError()
                            .json(serde_json::json!({"error": "Failed to store upload"})));
                    }
                    Err(StageError::Stream) => rejected.push(RejectedFile {
                        filename,
                        reason: "Upload stream interrupted".to_string(),
                    }),
                }
            }
            _ => {
                // Drain unknown fields so the stream stays in sync.
                let _ = read_text_field(&mut field).await;
            }
        }
    }

    let Some(user_id) = user_id else {
        bail!(HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "A valid user_id is required"})));
    };

    if title.is_empty() || location.is_empty() {
        bail!(HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "title and location are required"})));
    }

    if !price.is_finite() || price < 0.0 {
        bail!(HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "price must be a non-negative number"})));
    }

    // Distinguishing "unknown user" from a generic 500 keeps the client honest.
    let user_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(user_id)
            .fetch_one(&state.db)
            .await;

    match user_exists {
        Ok(true) => {}
        Ok(false) => {
            bail!(HttpResponse::BadRequest().json(serde_json::json!({"error": "Unknown user_id"})));
        }
        Err(e) => {
            error!("Failed to verify user {}: {}", user_id, e);
            bail!(HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "Failed to create property"})));
        }
    }

    let property_id = Uuid::new_v4();
    let description_opt = if description.is_empty() {
        None
    } else {
        Some(description)
    };

    if let Err(e) = sqlx::query(
        r#"INSERT INTO properties
           (id, title, location, price, description, bedrooms, bathrooms, area_sqm, user_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
    )
    .bind(property_id)
    .bind(&title)
    .bind(&location)
    .bind(price)
    .bind(&description_opt)
    .bind(bedrooms)
    .bind(bathrooms)
    .bind(area_sqm)
    .bind(user_id)
    .execute(&state.db)
    .await
    {
        error!("Failed to create property: {}", e);
        bail!(HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "Failed to create property"})));
    }

    let mut total_tokens = 0i64;
    let mut originals = 0usize;
    let mut duplicates = 0usize;
    let mut media: Vec<MediaView> = Vec::new();
    let mut first_image_url: Option<String> = None;

    for file in staged {
        let url = media_url(&file.content_hash, file.ext);
        let final_path = upload_dir.join(stored_filename(&file.content_hash, file.ext));

        // Claiming the hash and awarding tokens is one atomic step: whoever
        // wins the INSERT is the original uploader. A check-then-insert here
        // would let two concurrent uploads of the same file both be paid.
        let claimed = sqlx::query_scalar::<_, String>(
            r#"INSERT INTO media_content
               (content_hash, first_user_id, file_name, file_type, file_size)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (content_hash) DO NOTHING
               RETURNING content_hash"#,
        )
        .bind(&file.content_hash)
        .bind(user_id)
        .bind(stored_filename(&file.content_hash, file.ext))
        .bind(file.file_type)
        .bind(file.size)
        .fetch_optional(&state.db)
        .await;

        let is_original = match claimed {
            Ok(row) => row.is_some(),
            Err(e) => {
                // Previously this fell back to "treat as original", which paid
                // out tokens whenever the database hiccuped. Skip the file
                // instead of minting currency on an error path.
                error!("Failed to register media content: {}", e);
                let _ = async_fs::remove_file(&file.temp_path).await;
                rejected.push(RejectedFile {
                    filename: stored_filename(&file.content_hash, file.ext),
                    reason: "Storage error, file not saved".to_string(),
                });
                continue;
            }
        };

        // Content-addressed: an identical file already on disk is byte-for-byte
        // what we would have written, so keep it and drop the temp copy.
        let commit = if final_path.exists() {
            async_fs::remove_file(&file.temp_path).await
        } else {
            async_fs::rename(&file.temp_path, &final_path).await
        };

        if let Err(e) = commit {
            error!("Failed to commit upload {}: {}", url, e);
            let _ = async_fs::remove_file(&file.temp_path).await;
            rejected.push(RejectedFile {
                filename: stored_filename(&file.content_hash, file.ext),
                reason: "Storage error, file not saved".to_string(),
            });
            continue;
        }

        let tokens = if is_original {
            ORIGINAL_UPLOAD_TOKENS
        } else {
            0
        };
        let media_id = Uuid::new_v4();

        let linked = sqlx::query(
            r#"INSERT INTO media_uploads
               (id, property_id, user_id, file_path, file_type, content_hash, file_size, is_original, tokens_earned)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (property_id, content_hash) DO NOTHING"#,
        )
        .bind(media_id)
        .bind(property_id)
        .bind(user_id)
        .bind(&url)
        .bind(file.file_type)
        .bind(&file.content_hash)
        .bind(file.size)
        .bind(is_original)
        .bind(tokens)
        .execute(&state.db)
        .await;

        match linked {
            // Zero rows means the same file was sent twice in one request;
            // it is already attached, so there is nothing more to do.
            Ok(result) if result.rows_affected() == 0 => continue,
            Ok(_) => {}
            Err(e) => {
                error!("Failed to link media to property: {}", e);
                rejected.push(RejectedFile {
                    filename: stored_filename(&file.content_hash, file.ext),
                    reason: "Storage error, file not saved".to_string(),
                });
                continue;
            }
        }

        if is_original {
            if let Err(e) = award_tokens(&state.db, user_id, media_id, tokens).await {
                // The upload itself succeeded; surface the accounting failure
                // in the log rather than failing the whole request.
                error!("Failed to award tokens for {}: {}", media_id, e);
            } else {
                total_tokens += tokens;
            }
            originals += 1;
        } else {
            duplicates += 1;
        }

        if first_image_url.is_none() && file.file_type == "image" {
            first_image_url = Some(url.clone());
        }

        media.push(MediaView {
            url,
            file_type: file.file_type.to_string(),
        });
    }

    // Populate the legacy image columns so listings rendered straight from the
    // properties row show the uploader's own photo instead of a placeholder.
    if let Some(ref image_url) = first_image_url {
        if let Err(e) = sqlx::query(
            "UPDATE properties SET image_thumb_webp = $1, image_large_webp = $1 WHERE id = $2",
        )
        .bind(image_url)
        .bind(property_id)
        .execute(&state.db)
        .await
        {
            error!("Failed to set property cover image: {}", e);
        }
    }

    info!(
        "Property {} created: {} original, {} duplicate, {} rejected, {} tokens",
        property_id,
        originals,
        duplicates,
        rejected.len(),
        total_tokens
    );

    HttpResponse::Ok().json(UploadResponse {
        success: true,
        property_id,
        media,
        tokens_earned: total_tokens,
        originals,
        duplicates,
        message: build_upload_message(total_tokens, duplicates, rejected.len()),
        rejected,
    })
}

async fn award_tokens(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    amount: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query("UPDATE users SET token_balance = token_balance + $1 WHERE id = $2")
        .bind(amount)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO token_transactions (user_id, media_id, amount, transaction_type)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(media_id)
    .bind(amount)
    .bind("upload_reward")
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

// ============================================================================
// DEMO SEED
// ============================================================================

#[derive(Deserialize)]
struct SeedProperty {
    title: String,
    location: String,
    price: f64,
    description: Option<String>,
    image_thumb_webp: Option<String>,
    image_large_webp: Option<String>,
    bedrooms: Option<i32>,
    bathrooms: Option<i32>,
    area_sqm: Option<f64>,
}

/// Loads properties.json when the table is empty, so a fresh install shows a
/// populated gallery instead of an empty state. Enable with SEED_DEMO=true.
async fn seed_demo_data(pool: &PgPool) -> Result<(), sqlx::Error> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM properties")
        .fetch_one(pool)
        .await?;

    if count > 0 {
        info!("Skipping demo seed: {} properties already present", count);
        return Ok(());
    }

    let raw = match std::fs::read_to_string("properties.json") {
        Ok(r) => r,
        Err(e) => {
            warn!("SEED_DEMO set but properties.json could not be read: {}", e);
            return Ok(());
        }
    };

    let items: Vec<SeedProperty> = match serde_json::from_str(&raw) {
        Ok(i) => i,
        Err(e) => {
            warn!("properties.json is not valid seed data: {}", e);
            return Ok(());
        }
    };

    for item in &items {
        sqlx::query(
            r#"INSERT INTO properties
               (title, location, price, description, image_thumb_webp, image_large_webp,
                bedrooms, bathrooms, area_sqm)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
        )
        .bind(&item.title)
        .bind(&item.location)
        .bind(item.price)
        .bind(&item.description)
        .bind(&item.image_thumb_webp)
        .bind(&item.image_large_webp)
        .bind(item.bedrooms)
        .bind(item.bathrooms)
        .bind(item.area_sqm)
        .execute(pool)
        .await?;
    }

    info!("Seeded {} demo properties", items.len());
    Ok(())
}

// ============================================================================
// MAIN
// ============================================================================

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("JARVIS2026 v{} starting", env!("CARGO_PKG_VERSION"));

    let config = Config::from_env();

    if config.api_key.is_none() {
        warn!("=========================================================");
        warn!("API_KEY is not set — write endpoints are UNAUTHENTICATED.");
        warn!("Anyone who can reach this server can create users and");
        warn!("mint tokens. Set API_KEY before exposing it publicly.");
        warn!("=========================================================");
    }

    async_fs::create_dir_all(&config.upload_dir).await?;
    info!("Upload directory: {}", config.upload_dir.display());
    info!(
        "Per-file limit: {} MiB",
        config.max_file_bytes / (1024 * 1024)
    );

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/jarvis2026".to_string());

    info!("Connecting to database...");
    let pool = PgPoolOptions::new()
        .max_connections(
            std::env::var("DB_MAX_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
        )
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&database_url)
        .await
        .map_err(|e| std::io::Error::other(format!("database connection failed: {e}")))?;

    init_db(&pool)
        .await
        .map_err(|e| std::io::Error::other(format!("schema initialization failed: {e}")))?;

    if std::env::var("SEED_DEMO")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        if let Err(e) = seed_demo_data(&pool).await {
            warn!("Demo seeding failed: {}", e);
        }
    }

    // Railway, Render, Heroku and Fly all inject $PORT. Reading only
    // SERVER_PORT means the container listens on a port nothing routes to.
    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("SERVER_PORT"))
        .unwrap_or_else(|_| "8080".to_string());
    let host = std::env::var("SERVER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let bind_addr = format!("{}:{}", host, port);

    let upload_dir = config.upload_dir.clone();
    let allowed_origins = config.allowed_origins.clone();
    let app_state = web::Data::new(AppState { db: pool, config });

    info!("Listening on http://{}", bind_addr);
    info!("Allowed CORS origins: {:?}", allowed_origins);

    HttpServer::new(move || {
        let origins = allowed_origins.clone();
        let cors = Cors::default()
            .allowed_origin_fn(move |origin, req_head| {
                let Ok(origin) = origin.to_str() else {
                    return false;
                };
                let host = req_head
                    .headers
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .or_else(|| req_head.uri.authority().map(|a| a.as_str()));
                is_origin_allowed(origin, host, &origins)
            })
            .allowed_methods(vec!["GET", "POST", "OPTIONS"])
            .allowed_headers(vec![
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                header::HeaderName::from_static("x-api-key"),
            ])
            .max_age(3600);

        App::new()
            .wrap(cors)
            .wrap(middleware::Logger::default())
            .wrap(middleware::Compress::default())
            .app_data(app_state.clone())
            .app_data(web::PayloadConfig::new(512 * 1024 * 1024))
            .app_data(web::JsonConfig::default().limit(256 * 1024))
            .service(health_check)
            .service(get_properties)
            .service(get_property)
            .service(search_properties)
            .service(create_user)
            .service(get_user_balance)
            .service(get_user_transactions)
            .service(upload_property)
            .service(fs::Files::new("/uploads", &upload_dir))
            .service(fs::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(&bind_addr)?
    .run()
    .await
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_in_filenames() {
        // The dangerous part is the path, not the extension: these must never
        // yield anything that could be joined onto the upload directory.
        assert_eq!(safe_extension("../../static/app.js"), None);
        assert_eq!(safe_extension("../../../etc/passwd"), None);
        assert_eq!(safe_extension("..\\..\\windows\\system32\\cmd.exe"), None);
        assert_eq!(safe_extension("shell.php"), None);
        assert_eq!(safe_extension("payload.svg"), None);
        assert_eq!(safe_extension("page.html"), None);
    }

    #[test]
    fn accepts_allowlisted_media_extensions() {
        assert_eq!(safe_extension("photo.jpg"), Some(("jpg", "image")));
        assert_eq!(safe_extension("PHOTO.JPEG"), Some(("jpeg", "image")));
        assert_eq!(safe_extension("clip.MP4"), Some(("mp4", "video")));
        assert_eq!(safe_extension("render.webp"), Some(("webp", "image")));
    }

    #[test]
    fn extension_is_taken_from_the_final_segment_only() {
        // A traversal attempt with a valid-looking extension still must not
        // escape: only the extension is kept, the path is discarded.
        assert_eq!(safe_extension("../../evil.png"), Some(("png", "image")));
        let (ext, _) = safe_extension("../../evil.png").unwrap();
        let name = stored_filename("abc123", ext);
        assert!(!name.contains('/'), "stored name must not contain a path");
        assert!(
            !name.contains(".."),
            "stored name must not contain traversal"
        );
        assert_eq!(name, "abc123.png");
    }

    #[test]
    fn filenames_without_extensions_are_rejected() {
        assert_eq!(safe_extension("noextension"), None);
        assert_eq!(safe_extension(""), None);
        assert_eq!(safe_extension("."), None);
    }

    #[test]
    fn like_metacharacters_are_escaped() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("back\\slash"), "back\\\\slash");
        assert_eq!(escape_like("villa"), "villa");
    }

    #[test]
    fn upload_message_distinguishes_duplicates_from_rejections() {
        assert_eq!(
            build_upload_message(100, 0, 0),
            "Property published. You earned 100 tokens."
        );
        // The failure that mattered: rejected files must not be described as
        // duplicates, which made unsupported/oversized files look accepted.
        assert_eq!(
            build_upload_message(0, 0, 2),
            "Property published. 2 file(s) were rejected."
        );
        assert_eq!(
            build_upload_message(0, 1, 0),
            "Property published. 1 file(s) were already in the system and earned nothing."
        );
        assert_eq!(build_upload_message(0, 0, 0), "Property published.");
    }

    #[test]
    fn server_allows_its_own_origin() {
        // The bundled front-end is served by this binary, so its Origin is
        // whatever host the visitor typed. Refusing it broke every write from
        // the app's own UI on any host not listed in ALLOWED_ORIGINS.
        let allowlist = vec!["https://configured.example".to_string()];
        assert!(is_origin_allowed(
            "http://127.0.0.1:8099",
            Some("127.0.0.1:8099"),
            &allowlist
        ));
        assert!(is_origin_allowed(
            "https://app.up.railway.app",
            Some("app.up.railway.app"),
            &allowlist
        ));
    }

    #[test]
    fn configured_origins_are_allowed() {
        let allowlist = vec!["https://configured.example".to_string()];
        assert!(is_origin_allowed(
            "https://configured.example",
            Some("internal:8080"),
            &allowlist
        ));
    }

    #[test]
    fn foreign_origins_are_rejected() {
        let allowlist = vec!["https://configured.example".to_string()];
        assert!(!is_origin_allowed(
            "https://evil.example",
            Some("app.example"),
            &allowlist
        ));
        // A host suffix must not be enough: evil-app.example is not app.example.
        assert!(!is_origin_allowed(
            "https://evil-app.example",
            Some("app.example"),
            &allowlist
        ));
        assert!(!is_origin_allowed("https://evil.example", None, &allowlist));
    }

    #[test]
    fn media_urls_are_content_addressed() {
        assert_eq!(media_url("deadbeef", "jpg"), "/uploads/deadbeef.jpg");
    }

    #[test]
    fn page_limits_are_clamped() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(0)), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(-5)), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(100_000)), MAX_PAGE_LIMIT);
        assert_eq!(clamp_offset(Some(-1)), 0);
        assert_eq!(clamp_offset(Some(20)), 20);
    }

    #[test]
    fn allowed_origins_fall_back_to_localhost() {
        assert_eq!(parse_allowed_origins("").len(), 2);
        assert_eq!(parse_allowed_origins("   ").len(), 2);
        assert_eq!(
            parse_allowed_origins("https://a.com, https://b.com"),
            vec!["https://a.com".to_string(), "https://b.com".to_string()]
        );
    }
}
