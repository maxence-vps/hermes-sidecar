# hermes-sidecar

> Personal project released under the [GNU GPL v3](LICENSE). Copyright (C) 2026 Maxence Mottard.

Lightweight Rust API that runs next to a self-hosted [Hermes Agent](https://hermes-agent.nousresearch.com) instance and adds custom endpoints to it.

Hermes plugins can add tools, hooks and commands to the agent, but not HTTP routes to its API server. This sidecar fills that gap: clients call it with the same API key as Hermes, and it talks to Hermes over the internal Docker network.

```
App ──HTTPS──▶ reverse proxy ──▶ sidecar (:8080) ──HTTP, internal network──▶ Hermes (:8642)
```

## Authentication

Every route except `/health` requires `Authorization: Bearer <API_SERVER_KEY>`, the same key Hermes uses. The sidecar reads it from its environment (point both services at the same `.env`) and compares it in constant time. It does not ask Hermes to validate the key: Hermes `GET /health` answers without checking it.

## Configuration

| Variable | Required | Default | Description |
|---|---|---|---|
| `API_SERVER_KEY` | yes | | Key shared with Hermes. The sidecar refuses to start without it. |
| `HERMES_URL` | yes | | Hermes base URL on the internal network, e.g. `http://hermes:8642`. |
| `DATABASE_PATH` | yes | | Absolute path of the sidecar's SQLite file, created on first start, e.g. `/data/sidecar.db`. Put it on a volume. See [Database](#database). |
| `BIND_ADDR` | no | `0.0.0.0:8080` | Listen address. |
| `HERMES_MEDIA_DIR` | no | | Hermes `native-inputs` folder, mounted read-only, that `GET /media/{sha256}` reads. Must be absolute. Unset, the route answers `503`. See [Images](#images). |
| `API_DOCS_ENABLED` | no | `false` | Serves the OpenAPI spec and the Scalar docs (`true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off`). Any other value stops startup. |
| `RUST_LOG` | no | `info` | Log filter. |

## Endpoints

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/health` | no | Liveness probe. Does not call Hermes. |
| `GET` | `/status` | yes | Checks the key and that Hermes is reachable. `502` if Hermes is down. |
| `GET` | `/media/{sha256}` | yes | Image a client attached to a message. See [Images](#images). |
| `GET` | `/projects` | yes | Every project, sorted by name ignoring case. See [Projects](#projects). |
| `POST` | `/projects` | yes | Creates a project from `{"name"}`. `201`. |
| `GET` | `/projects/{id}` | yes | One project. |
| `PATCH` | `/projects/{id}` | yes | Renames a project from `{"name"}`. |
| `DELETE` | `/projects/{id}` | yes | Deletes a project and its links. `204`. |
| `POST` | `/projects/{id}/repositories` | yes | Attaches a repository from `{"url"}`. `201`, `{id, name, url}`. |
| `DELETE` | `/projects/{id}/repositories/{repository_id}` | yes | Detaches a repository. `204`. |
| `GET` | `/openapi.json` | no | OpenAPI spec. Only when `API_DOCS_ENABLED=true`. |
| `GET` | `/docs` | no | [Scalar](https://scalar.com) API reference. Only when `API_DOCS_ENABLED=true`. |

Behind the reverse proxy the sidecar is served under a path prefix (for example `https://hermes-api.example.test/sidecar/status`); the proxy strips the prefix, so routes here have none.

Errors are JSON: `{"error": {"code": "...", "message": "..."}}`. A request body that is not the expected JSON is answered with `invalid_body` (`400` for malformed JSON, `415` without `Content-Type: application/json`, `422` for a missing or mistyped field), and a database failure with `500 database_error`.

## Images

Hermes does not return the images clients send. It stores each one as `<native-inputs>/<sha256>/<name>`, the folder name being the SHA-256 of the file, and keeps only the line `[Image attached at: <path>]` in the message text. Its API server has no route that returns the file (the dashboard has `GET /api/media`, but behind its own session token).

`GET /media/{sha256}` fills that gap. The client takes the folder name from the `[Image attached at: …]` path and gets the image back:

- Only PNG, JPEG, GIF and WebP files are served (no SVG), up to 25 MiB, with their `Content-Type` and `X-Content-Type-Options: nosniff`.
- The sidecar hashes the file before sending it. A file whose bytes no longer match its folder name is a `404`, like a missing one.
- Symlinks are ignored, both for the folder and the file.
- `ETag` is the quoted digest and `Cache-Control` is `private, max-age=31536000, immutable`, since the bytes of a digest never change. `If-None-Match` gets a `304` while the image still exists.
- Hermes deletes media it no longer needs, so an old image can become a `404`.

The sidecar reads the files from the Hermes data volume:

- Mount only `cache/documents/native-inputs` (volume `subpath`, Docker 26+), read-only, so the sidecar does not see the rest of the Hermes data (`.env`, `auth.json`).
- Run the sidecar as the Hermes user (`10000:10000` in the official image): the files are `0600`.
- Set `HERMES_MEDIA_DIR` to the mount point.

See `docker-compose.example.yml`.

## Database

The sidecar keeps its own data (projects, see `GLOSSARY.md`) in a SQLite file at `DATABASE_PATH`, never in Hermes's database. See `docs/adr/0001-sqlite-for-sidecar-data.md`.

- The file is created if missing, in WAL mode (SQLite also writes `-wal` and `-shm` files next to it, so mount a directory, not the file).
- The migrations in `migrations/` are embedded in the binary and applied at startup. Startup fails if the file cannot be opened or migrated.
- The image creates `/data` owned by `10000:10000`, so a new named volume mounted there is writable by the sidecar.

## Projects

A Project groups Conversations (Hermes sessions) and the git Repositories they relate to. Projects only organise: Hermes knows nothing about them. Vocabulary: `GLOSSARY.md`.

A project is returned as:

```json
{
  "id": "0b7e8f5c-3a3e-4f0e-9a8e-3f1c2d4b5a69",
  "name": "Hermes",
  "repositories": [],
  "conversation_ids": []
}
```

- `id` is a UUID generated by the sidecar. Any other value in a URL is a `404 project_not_found`.
- `name` is trimmed, not empty, at most 100 characters, and unique ignoring case (Unicode included). Renaming a project to its own name with another case is allowed. Errors: `422 invalid_name`, `409 project_name_taken`.
- `repositories` lists the attached git repositories as `{id, name, url}`, sorted by name ignoring case. See [Repositories](#repositories).
- Deleting a project removes its links to repositories and conversations. It never touches Hermes: the conversations stay there, without a project.

### Repositories

`POST /projects/{id}/repositories` takes `{"url": "..."}` in one of these forms, and stores it as `https://host/path`:

- `https://host/owner/name`
- `git@host:owner/name`
- `ssh://git@host/owner/name` (the SSH port is dropped)

The host is lowercased and a trailing `.git` or `/` is removed. The path keeps its case, since GitLab and Gitea paths are case-sensitive. `http://`, `file://`, local paths, credentials, queries and paths with fewer than two segments are rejected with `422 invalid_repository_url`.

The name is the whole path after the host, never sent by the client: `https://gitlab.com/org/team/api` gives `org/team/api`.

A project has each URL at most once (`409 repository_already_attached`); the same URL may be attached to several projects, with one id per link. There is no update route: delete the repository and add it again. `DELETE` answers `404 repository_not_found` for an id that is not in this project.

## OpenAPI docs

The spec is generated at compile time with [utoipa](https://github.com/juhaku/utoipa) from the `#[utoipa::path]` annotations on the handlers, so a route registered with `routes!(...)` is always documented. The Scalar page embeds the spec and loads Scalar itself from a CDN, so the binary barely grows.

Behind a proxy that strips a path prefix, the spec's server URL is taken from the `X-Forwarded-Prefix` header (set by Traefik's strip-prefix middleware), so "Try it" calls `/sidecar/status` and not `/status`.

## Adding an endpoint

Write the handler in `src/routes.rs` (or in its own module with a `router()` merged into `protected`, like `src/projects.rs`) with a `#[utoipa::path(...)]` annotation (add `security(("bearer" = []))` for protected routes) and `ToSchema` response types, then register it in the `protected` router with `routes!(...)`. Call Hermes through `HermesClient` (`src/hermes.rs`).

## Development

Tools are pinned with [mise](https://mise.jdx.dev).

```sh
mise install          # Rust toolchain
mise run test         # tests
mise run lint         # rustfmt + clippy
API_SERVER_KEY=dev-key HERMES_URL=http://localhost:8642 DATABASE_PATH="$PWD/sidecar.db" mise run run
```

## Docker

The image is a static musl binary in an empty (`scratch`) image, with no TLS library: the sidecar only talks to Hermes in plain HTTP on the internal network, and HTTPS is handled by the reverse proxy. It runs as `10000:10000`, the Hermes user.

`.github/workflows/build.yml` builds the image and pushes it to `ghcr.io/maxence-vps/hermes-sidecar` on every push to `main`, tagged `latest` and `sha-<commit>`. Pin a `sha-` tag in production.

```sh
docker build -t hermes-sidecar .
```

See `docker-compose.example.yml` for running it next to Hermes.

## License

Copyright (C) 2026 Maxence Mottard.

This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed without any warranty. See [LICENSE](LICENSE).
