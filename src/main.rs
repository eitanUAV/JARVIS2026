// JARVIS2026 - AI Property Finder + Video Upload System
// by Mikhael Abraham | +6281280126126
// Date: January 14, 2026

use actix_cors::Cors;
use actix_files as fs;
use actix_multipart::Multipart;
use actix_web::{get, middleware, post, web, App, HttpResponse, HttpServer, Responder};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::fs as async_fs;
use tokio::io::AsyncWriteExt;
use tracing::{error, info};
use uuid::Uuid;

// ============================================================================
// DATA STRUCTURES
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug, sqlx::FromRow)]
struct Property {
    id: Uuid,
    title: String,
    location: String,
    price: f64,
    description: String,
    image_thumb_webp: String,
    image_large_webp: String,
    bedrooms: Option<i32>,
    bathrooms: Option<i32>,
    area_sqm: Option<f64>,
    user_id: Option<Uuid>,
    content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
struct User {
    id: Uuid,
    username: String,
    wallet_address: Option<String>,
    token_balance: i64,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
struct MediaUpload {
    id: Uuid,
    property_id: Uuid,
    user_id: Uuid,
    file_path: String,
    file_type: String,
    content_hash: String,
    file_size: i64,
    is_original: bool,
    tokens_earned: i64,
    uploaded_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
struct UploadResponse {
    success: bool,
    property_id: Uuid,
    media_ids: Vec<Uuid>,
    tokens_earned: i64,
    message: String,
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
    wallet_address: Option<String>,
}

#[derive(Deserialize)]
struct SearchQuery {
    query: String,
}

struct AppState {
    db: PgPool,
}

const ORIGINAL_UPLOAD_TOKENS: i64 = 100;

// ============================================================================
// DATABASE INITIALIZATION
// ============================================================================

async fn init_db(pool: &PgPool) -> Result<(), sqlx::Error> {
    info!("Initializing database schema...");

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS users (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            username TEXT UNIQUE NOT NULL,
            wallet_address TEXT,
            token_balance BIGINT DEFAULT 0,
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

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS media_uploads (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            property_id UUID REFERENCES properties(id) ON DELETE CASCADE,
            user_id UUID REFERENCES users(id),
            file_path TEXT NOT NULL,
            file_type TEXT NOT NULL,
            content_hash TEXT UNIQUE NOT NULL,
            file_size BIGINT NOT NULL,
            is_original BOOLEAN DEFAULT true,
            tokens_earned BIGINT DEFAULT 0,
            uploaded_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS token_transactions (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            user_id UUID REFERENCES users(id),
            media_id UUID REFERENCES media_uploads(id),
            amount BIGINT NOT NULL,
            transaction_type TEXT NOT NULL,
            created_at TIMESTAMPTZ DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_media_content_hash ON media_uploads(content_hash)")
        .execute(pool)
        .await?;

    info!("Database schema initialized successfully");
    Ok(())
}

// ============================================================================
// UTILITY FUNCTIONS
// ============================================================================

async fn calculate_file_hash(file_data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_data);
    hex::encode(hasher.finalize())
}

async fn check_duplicate(pool: &PgPool, content_hash: &str) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_uploads WHERE content_hash = $1")
            .bind(content_hash)
            .fetch_one(pool)
            .await?;
    Ok(result > 0)
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
        "INSERT INTO token_transactions (user_id, media_id, amount, transaction_type) VALUES ($1, $2, $3, $4)"
    )
    .bind(user_id)
    .bind(media_id)
    .bind(amount)
    .bind("upload_reward")
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

// ============================================================================
// API HANDLERS
// ============================================================================

#[get("/api/health")]
async fn health_check() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "service": "JARVIS2026",
        "version": "1.0.0"
    }))
}

#[get("/api/properties")]
async fn get_properties(state: web::Data<AppState>) -> impl Responder {
    match sqlx::query_as::<_, Property>("SELECT * FROM properties ORDER BY created_at DESC")
        .fetch_all(&state.db)
        .await
    {
        Ok(props) => HttpResponse::Ok().json(props),
        Err(e) => {
            error!("Failed to fetch properties: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to fetch properties"
            }))
        }
    }
}

#[post("/api/search")]
async fn search_properties(
    query: web::Json<SearchQuery>,
    state: web::Data<AppState>,
) -> impl Responder {
    let search = format!("%{}%", query.query.to_lowercase());

    match sqlx::query_as::<_, Property>(
        "SELECT * FROM properties WHERE
         LOWER(title) LIKE $1 OR
         LOWER(location) LIKE $1 OR
         LOWER(description) LIKE $1
         ORDER BY created_at DESC",
    )
    .bind(&search)
    .fetch_all(&state.db)
    .await
    {
        Ok(results) => {
            info!("Search '{}' found {} results", query.query, results.len());
            HttpResponse::Ok().json(results)
        }
        Err(e) => {
            error!("Search failed: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Search failed"
            }))
        }
    }
}

#[post("/api/users")]
async fn create_user(
    req: web::Json<CreateUserRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    match sqlx::query_as::<_, User>(
        "INSERT INTO users (username, wallet_address) VALUES ($1, $2) RETURNING *",
    )
    .bind(&req.username)
    .bind(&req.wallet_address)
    .fetch_one(&state.db)
    .await
    {
        Ok(user) => {
            info!("User created: {} ({})", user.username, user.id);
            HttpResponse::Ok().json(user)
        }
        Err(e) => {
            error!("Failed to create user: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to create user"
            }))
        }
    }
}

#[get("/api/users/{user_id}/balance")]
async fn get_user_balance(path: web::Path<Uuid>, state: web::Data<AppState>) -> impl Responder {
    let user_id = path.into_inner();

    match sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(user) => HttpResponse::Ok().json(user),
        Err(_) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "User not found"
        })),
    }
}

#[post("/api/upload-property")]
async fn upload_property(mut payload: Multipart, state: web::Data<AppState>) -> impl Responder {
    let mut user_id: Option<Uuid> = None;
    let mut title = String::new();
    let mut location = String::new();
    let mut price = 0.0;
    let mut description = String::new();
    let mut bedrooms: Option<i32> = None;
    let mut bathrooms: Option<i32> = None;
    let mut area_sqm: Option<f64> = None;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    while let Some(item) = payload.next().await {
        let mut field = match item {
            Ok(f) => f,
            Err(_) => continue,
        };

        let name = field.name().to_string();

        match name.as_str() {
            "user_id" => {
                if let Some(Ok(chunk)) = field.next().await {
                    if let Ok(s) = String::from_utf8(chunk.to_vec()) {
                        user_id = Uuid::parse_str(&s).ok();
                    }
                }
            }
            "title" => {
                if let Some(Ok(chunk)) = field.next().await {
                    title = String::from_utf8(chunk.to_vec()).unwrap_or_default();
                }
            }
            "location" => {
                if let Some(Ok(chunk)) = field.next().await {
                    location = String::from_utf8(chunk.to_vec()).unwrap_or_default();
                }
            }
            "price" => {
                if let Some(Ok(chunk)) = field.next().await {
                    if let Ok(s) = String::from_utf8(chunk.to_vec()) {
                        price = s.parse().unwrap_or(0.0);
                    }
                }
            }
            "description" => {
                if let Some(Ok(chunk)) = field.next().await {
                    description = String::from_utf8(chunk.to_vec()).unwrap_or_default();
                }
            }
            "bedrooms" => {
                if let Some(Ok(chunk)) = field.next().await {
                    if let Ok(s) = String::from_utf8(chunk.to_vec()) {
                        bedrooms = s.parse().ok();
                    }
                }
            }
            "bathrooms" => {
                if let Some(Ok(chunk)) = field.next().await {
                    if let Ok(s) = String::from_utf8(chunk.to_vec()) {
                        bathrooms = s.parse().ok();
                    }
                }
            }
            "area_sqm" => {
                if let Some(Ok(chunk)) = field.next().await {
                    if let Ok(s) = String::from_utf8(chunk.to_vec()) {
                        area_sqm = s.parse().ok();
                    }
                }
            }
            "files" => {
                let filename = field
                    .content_disposition()
                    .get_filename()
                    .unwrap_or("upload")
                    .to_string();

                let mut file_data = Vec::new();
                while let Some(chunk) = field.next().await {
                    if let Ok(data) = chunk {
                        file_data.extend_from_slice(&data);
                    }
                }
                files.push((filename, file_data));
            }
            _ => {}
        }
    }

    let user_id = match user_id {
        Some(id) => id,
        None => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "user_id required"}))
        }
    };

    let property_id = Uuid::new_v4();

    let result = sqlx::query(
        r#"INSERT INTO properties
        (id, title, location, price, description, bedrooms, bathrooms, area_sqm, user_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
    )
    .bind(property_id)
    .bind(&title)
    .bind(&location)
    .bind(price)
    .bind(&description)
    .bind(bedrooms)
    .bind(bathrooms)
    .bind(area_sqm)
    .bind(user_id)
    .execute(&state.db)
    .await;

    if result.is_err() {
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "Failed to create property"}));
    }

    let mut total_tokens = 0i64;
    let mut media_ids = Vec::new();

    for (filename, file_data) in files {
        let content_hash = calculate_file_hash(&file_data).await;
        let is_duplicate = check_duplicate(&state.db, &content_hash)
            .await
            .unwrap_or(false);
        let is_original = !is_duplicate;
        let tokens = if is_original {
            ORIGINAL_UPLOAD_TOKENS
        } else {
            0
        };

        async_fs::create_dir_all("uploads").await.ok();
        let file_path = format!("uploads/{}", filename);
        let mut file = async_fs::File::create(&file_path).await.unwrap();
        file.write_all(&file_data).await.ok();

        let file_type = if filename.ends_with(".mp4") || filename.ends_with(".mov") {
            "video"
        } else {
            "image"
        };

        let media_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO media_uploads
            (id, property_id, user_id, file_path, file_type, content_hash, file_size, is_original, tokens_earned)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#
        )
        .bind(media_id)
        .bind(property_id)
        .bind(user_id)
        .bind(&file_path)
        .bind(file_type)
        .bind(&content_hash)
        .bind(file_data.len() as i64)
        .bind(is_original)
        .bind(tokens)
        .execute(&state.db)
        .await.ok();

        if is_original {
            award_tokens(&state.db, user_id, media_id, tokens)
                .await
                .ok();
            total_tokens += tokens;
        }

        media_ids.push(media_id);
    }

    info!(
        "Property uploaded: {} - {} tokens earned",
        property_id, total_tokens
    );

    HttpResponse::Ok().json(UploadResponse {
        success: true,
        property_id,
        media_ids,
        tokens_earned: total_tokens,
        message: format!("Property created! Earned {} tokens", total_tokens),
    })
}

// ============================================================================
// MAIN
// ============================================================================

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    info!("╔═══════════════════════════════════════════════════════╗");
    info!("║           🤖 JARVIS2026 Starting...                  ║");
    info!("║     by Mikhael Abraham | +6281280126126              ║");
    info!("╚═══════════════════════════════════════════════════════╝");

    dotenv::dotenv().ok();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/jarvis2026".to_string());

    info!("Connecting to database...");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    init_db(&pool).await.expect("Failed to initialize database");

    let app_state = web::Data::new(AppState { db: pool });

    let host = std::env::var("SERVER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("SERVER_PORT").unwrap_or_else(|_| "8080".to_string());
    let bind_addr = format!("{}:{}", host, port);

    info!("🚀 Server starting on http://{}", bind_addr);
    info!("📡 API endpoints available at /api/*");
    info!("🎙️  Voice commands ready");
    info!("📹 Video upload with token rewards enabled");
    info!("");

    HttpServer::new(move || {
        let cors = Cors::default()
            .allowed_origin("https://sultanproperti.com")
            .allowed_origin("http://sultanproperti.com")
            .allowed_origin("http://localhost:8080")
            .allowed_origin("http://127.0.0.1:8080")
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .wrap(cors)
            .wrap(middleware::Logger::default())
            .app_data(app_state.clone())
            .app_data(web::PayloadConfig::new(500 * 1024 * 1024))
            .service(health_check)
            .service(get_properties)
            .service(search_properties)
            .service(create_user)
            .service(get_user_balance)
            .service(upload_property)
            .service(fs::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(&bind_addr)?
    .run()
    .await
}
