# JARVIS2026

A property listing API with media upload and a token reward system, written in
Rust (Actix-web + PostgreSQL). Ships with a browser front-end.

Sellers publish a listing with photos and video. Each genuinely new file earns
the uploader tokens; files already known to the system earn nothing but are
still attached to the listing. Media is stored content-addressed, so identical
files are stored once no matter how many listings reference them.

---

## Quick start

```bash
cp .env.example .env          # then set DATABASE_URL and API_KEY
make db-up                    # PostgreSQL via Docker
cargo run --release
```

Open <http://localhost:8080>.

To start with a populated gallery, set `SEED_DEMO=true` — `properties.json` is
loaded on first boot when the properties table is empty.

The schema is created automatically at startup; there is no separate migration
step.

---

## Configuration

All settings are environment variables. See `.env.example` for a complete file.

| Variable | Default | Purpose |
|---|---|---|
| `DATABASE_URL` | `postgres://postgres:password@localhost:5432/jarvis2026` | PostgreSQL connection string |
| `API_KEY` | *(unset)* | Shared secret for write endpoints. **Unset means writes are unauthenticated.** |
| `ALLOWED_ORIGINS` | `http://localhost:8080,http://127.0.0.1:8080` | Comma-separated CORS origins |
| `PORT` / `SERVER_PORT` | `8080` | Listen port. `PORT` wins — hosting platforms inject it |
| `SERVER_HOST` | `127.0.0.1` | Bind address. The Docker image sets `0.0.0.0` |
| `UPLOAD_DIR` | `uploads` | Where media is stored |
| `MAX_FILE_MB` | `25` | Per-file upload ceiling |
| `DB_MAX_CONNECTIONS` | `10` | Connection pool size |
| `SEED_DEMO` | `false` | Load `properties.json` when the table is empty |
| `RUST_LOG` | `info` | Log filter |

### Authentication

`POST /api/users` and `POST /api/upload-property` require the API key when
`API_KEY` is set, sent as either header:

```
X-API-Key: <key>
Authorization: Bearer <key>
```

Read endpoints are always public. **If `API_KEY` is unset the write endpoints
are open**, meaning anyone who can reach the server can create users and mint
tokens; the server logs a warning at startup in that case. Generate a key with
`openssl rand -hex 32`.

The bundled front-end stores a key in `localStorage` — use the gear icon in the
top bar to set it. That is appropriate for an operator-facing demo, not for
untrusted public users; see [Before you sell it](#before-you-sell-it).

---

## API

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/api/health` | – | Liveness plus a real database check |
| `GET` | `/api/properties?limit=&offset=` | – | List properties, newest first, with attached media |
| `GET` | `/api/properties/{id}` | – | A single property with its media |
| `POST` | `/api/search` | – | `{"query": "bali", "limit": 20, "offset": 0}` |
| `POST` | `/api/users` | key | `{"username": "...", "wallet_address": "..."}` |
| `GET` | `/api/users/{id}/balance` | – | Token balance |
| `GET` | `/api/users/{id}/transactions` | – | Token history |
| `POST` | `/api/upload-property` | key | `multipart/form-data`, see below |
| `GET` | `/uploads/{file}` | – | Stored media |

`limit` defaults to 50 and is capped at 200.

### Uploading

`POST /api/upload-property` takes `multipart/form-data`:

`user_id` (required, UUID), `title` (required), `location` (required), `price`,
`description`, `bedrooms`, `bathrooms`, `area_sqm`, and repeated `files` fields.

Accepted media: `jpg`, `jpeg`, `png`, `webp`, `gif`, `avif`, `mp4`, `mov`,
`m4v`, `webm`. Up to 20 files per request, each within `MAX_FILE_MB`.
Anything else is reported in `rejected` rather than silently dropped.

```json
{
  "success": true,
  "property_id": "…",
  "media": [{ "url": "/uploads/<sha256>.png", "file_type": "image" }],
  "tokens_earned": 100,
  "originals": 1,
  "duplicates": 1,
  "rejected": [{ "filename": "notes.txt", "reason": "Unsupported file type" }],
  "message": "Property published. You earned 100 tokens. 1 file(s) were already in the system and earned nothing."
}
```

The response reports each file's outcome. A `200` does not mean every file was
stored — check `rejected`.

---

## How media and tokens work

Uploads are streamed to a temporary file while being hashed, never buffered
whole in memory, and are rejected the moment they exceed the size limit.

The stored filename is `<sha256>.<ext>`, where the extension comes from a fixed
allowlist rather than from the client. The uploaded filename never reaches the
filesystem path.

`media_content` holds one row per distinct file hash, with `content_hash` as its
primary key. The first uploader of a hash wins an
`INSERT … ON CONFLICT DO NOTHING` and earns `ORIGINAL_UPLOAD_TOKENS` (100);
everyone after gets zero. Because that decision is a single atomic statement,
two simultaneous uploads of the same file cannot both be paid.

`media_uploads` links content to listings, unique on `(property_id,
content_hash)`. The same photo can appear in several listings while occupying
one copy on disk.

---

## Deployment

The image builds dependencies as a separate cached layer, runs as an
unprivileged user, and includes a `HEALTHCHECK`.

```bash
make docker-build
docker run -p 8080:8080 \
  -e DATABASE_URL='postgres://…' \
  -e API_KEY='…' \
  -e ALLOWED_ORIGINS='https://yourdomain.com' \
  -v jarvis-uploads:/app/uploads \
  jarvis2026:latest
```

`railway.json` is set up for Railway; `PORT` is read automatically. Any platform
that builds a Dockerfile and injects `PORT` will work.

**Mount a volume at `UPLOAD_DIR`.** Container filesystems are discarded on
redeploy, and uploaded media goes with them. This is the single most common way
to lose customer data with this service.

---

## Development

```bash
make test     # unit tests
make lint     # clippy, warnings denied
make check    # what CI runs: fmt + clippy + test
```

CI runs on every push and pull request (`.github/workflows/ci.yml`).

---

## Before you sell it

Honest list of what is not built yet. None of it is required to run the
service; all of it matters depending on who you sell to.

- **No user authentication.** `API_KEY` is a single shared operator secret, not
  per-user login. Any holder of the key can upload as any `user_id`. A
  multi-tenant product needs real accounts, sessions, and per-user authorisation.
- **Tokens are database rows.** `wallet_address` is stored but never used. There
  is no chain, no transfer, no redemption, and no withdrawal path. Describe it
  as a points system unless you build one.
- **Local disk storage.** Media is served from the application's own filesystem.
  Object storage (S3, R2, Spaces) is the usual next step for durability, and a
  CDN for delivery.
- **Uploads are not transcoded.** Files are served at their original resolution;
  the `image_thumb_webp` and `image_large_webp` columns both hold the same URL.
  Real thumbnailing needs an image pipeline.
- **File contents are not validated.** The extension is allowlisted, but the
  bytes are not verified to match it. Executable formats are excluded and media
  is served from the same origin as the app, so consider an image-decode check
  or a separate media domain before accepting uploads from the public.
- **No rate limiting.** Nothing throttles uploads or user creation.
- **No moderation.** Listing text is stored as submitted, escaped at render
  time. There is no review queue or takedown flow.
- **No deletion.** Neither listings nor media can be removed through the API,
  which matters for GDPR/UU PDP erasure requests.

## Licence

Proprietary — all rights reserved. See [LICENSE](LICENSE).
