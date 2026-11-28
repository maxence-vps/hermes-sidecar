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
| `GET` | `/models` | yes | Model aliases Hermes serves. See [Models](#models). |
| `GET` | `/models/options` | yes | Models grouped by provider, with `include_unconfigured`. See [Models](#models). |
| `GET` | `/sessions` | yes | Lists Hermes sessions, with `limit` and `offset`. See [Sessions](#sessions). |
| `POST` | `/sessions` | yes | Creates a Hermes session from `{"model", "provider"}` (both optional). |
| `GET` | `/sessions/{id}` | yes | One Hermes session. |
| `PATCH` | `/sessions/{id}` | yes | Renames a Hermes session from `{"title"}`. |
| `DELETE` | `/sessions/{id}` | yes | Deletes a Hermes session and takes it out of its project. |
| `POST` | `/sessions/{id}/fork` | yes | Forks a Hermes session. The fork takes the original's place in its project. |
| `POST` | `/sessions/{id}/chat/stream` | yes | Relays an agent turn from Hermes. Decoupled from the client. See [Chat streams](#chat-streams). |
| `GET` | `/sessions/{id}/stream/status` | yes | Active stream status for a conversation. See [Chat streams](#chat-streams). |
| `GET` | `/sessions/{id}/stream` | yes | Attaches to an ongoing stream, replaying buffered events. See [Chat streams](#chat-streams). |
| `GET` | `/projects` | yes | Every project, sorted by name ignoring case. See [Projects](#projects). |
| `POST` | `/projects` | yes | Creates a project from `{"name"}`. `201`. |
| `GET` | `/projects/{id}` | yes | One project. |
| `PATCH` | `/projects/{id}` | yes | Renames a project from `{"name"}`. |
| `DELETE` | `/projects/{id}` | yes | Deletes a project and its links. `204`. |
| `POST` | `/projects/{id}/repositories` | yes | Attaches a repository from `{"url"}`. `201`, `{id, name, url}`. |
| `DELETE` | `/projects/{id}/repositories/{repository_id}` | yes | Detaches a repository. `204`. |
| `PUT` | `/projects/{id}/conversations/{conversation_id}` | yes | Puts a conversation in a project, moving it from another one. `204`. |
| `DELETE` | `/projects/{id}/conversations/{conversation_id}` | yes | Takes a conversation out of a project. `204`. |
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

## Models

The model catalog goes through the sidecar, for the client's settings screen and model picker. Each route answers with Hermes's JSON body as is. Nothing is cached, so a provider configured in Hermes shows up at once.

- `GET /models` calls Hermes `GET /v1/models`: the model aliases (`hermes-agent`, …).
- `GET /models/options` calls Hermes `GET /api/model/options`: the models grouped by provider (OpenRouter, Anthropic, OpenAI, …). It forwards `include_unconfigured` (`true` or `false`, optional) and no other parameter; left out, Hermes uses its default.

Errors:

- `400 invalid_request` when `include_unconfigured` is neither `true` nor `false`.
- `502 hermes_unavailable` when Hermes is unreachable, fails, or answers with something that is not JSON.

## Sessions

The session lifecycle goes through the sidecar, so clients never need to reach Hermes (`:8642`) directly. Each route calls the matching Hermes route (`/api/sessions…`) and answers with Hermes's status and JSON body as is; the sidecar only adds the side effects on [projects](#conversations).

- `GET /sessions` forwards `limit` and `offset` (non-negative integers, both optional) and no other parameter. A value that is not a non-negative integer is `400 invalid_request`.
- `POST /sessions` forwards `model` and `provider`; a field left out is not sent, so Hermes uses its default. Send `{}` for both defaults.
- `PATCH /sessions/{id}` forwards `{"title": "..."}`. A missing or non-string `title` is `422 invalid_body`.
- `DELETE /sessions/{id}` takes the session out of its project once Hermes has deleted it, and also when Hermes answers `404` (the session was already gone), so it does not stay referenced. Any other Hermes failure keeps the link.
- `POST /sessions/{id}/fork` forwards its body, if any, as is (it must be a JSON object). The new id is read from `session.id`, or else `id`, in Hermes's answer. If the original was in a project, the fork goes into that project and the original leaves it. Without a usable new id, projects are left unchanged.

Errors:

- `422 invalid_conversation_id` for an id that is not 1 to 255 printable ASCII characters, without spaces or `/`, or is `.` or `..`.
- `404 session_not_found` when Hermes has no session with this id.
- `400 invalid_request` when Hermes rejects the request (`400` or `422`), or the fork body is not a JSON object; `400`/`415`/`422 invalid_body` for a create or rename body that is not the expected JSON.
- `502 hermes_unavailable` when Hermes is unreachable, fails, or answers with something that is not JSON.
- `500 database_error` when Hermes succeeded but the project could not be updated: the Hermes change is not rolled back.

## Chat streams

When a client initiates an agent turn directly through Hermes `POST /api/sessions/{id}/chat/stream`, Hermes interrupts and cancels the turn if the client HTTP connection drops (such as when iOS backgrounds the app or switches networks).

`POST /sessions/{id}/chat/stream` proxies the request to Hermes and decouples the connection:

- Accepts `{"message": ...}` (same schema as Hermes).
- Runs in a background task that reads Hermes's SSE stream to completion, even if the client disconnects mid-stream.
- Buffers events of the ongoing turn in memory and streams them back to the caller as Server-Sent Events (`run.started`, `assistant.delta`, `tool.*`, `assistant.completed`, `run.*`, `done`).
- Returns `404 session_not_found` if the session does not exist, `400 invalid_request` if the body is invalid, and `502 hermes_unavailable` if Hermes cannot be reached. Multiple turns in the same session are queued and relayed in order.

`GET /sessions/{id}/stream/status` checks whether an agent turn is currently running for this conversation:
- Returns `{"is_active": bool, "message_id": string | null, "started_at": string | null}`.

`GET /sessions/{id}/stream` allows a client to attach to an ongoing turn (e.g. on reconnection or from another device):
- If active, returns `200 OK` (`text/event-stream`), replays buffered events in order (or events after `Last-Event-ID` if provided), and continues streaming live events until `done`.
- If no stream is active, returns `404 stream_not_active`.

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
- `conversation_ids` lists the Hermes session ids in the project, in no guaranteed order. See [Conversations](#conversations).
- Deleting a project removes its links to repositories and conversations. It never touches Hermes: the conversations stay there, without a project.

### Repositories

`POST /projects/{id}/repositories` takes `{"url": "..."}` in one of these forms, and stores it as `https://host/path`:

- `https://host/owner/name`
- `git@host:owner/name`
- `ssh://git@host/owner/name` (the SSH port is dropped)

The host is lowercased and a trailing `.git` or `/` is removed. The path keeps its case, since GitLab and Gitea paths are case-sensitive. `http://`, `file://`, local paths, credentials, queries and paths with fewer than two segments are rejected with `422 invalid_repository_url`.

The name is the whole path after the host, never sent by the client: `https://gitlab.com/org/team/api` gives `org/team/api`.

A project has each URL at most once (`409 repository_already_attached`); the same URL may be attached to several projects, with one id per link. There is no update route: delete the repository and add it again. `DELETE` answers `404 repository_not_found` for an id that is not in this project.

### Conversations

A Conversation is a Hermes session, referenced by its id only: the sidecar stores no title, date or message, so the client sorts `conversation_ids` with what it knows from Hermes. A conversation is in at most one project, or none.

- `PUT /projects/{id}/conversations/{conversation_id}` puts it in the project. If it was in another project, it moves. Calling it again changes nothing.
- `DELETE` takes it out of the project, and answers `204` also when it was not there (a conversation in another project stays there).
- The id is 1 to 255 printable ASCII characters, without spaces or `/` (even percent-encoded). Anything else is `422 invalid_conversation_id`.
- The sidecar does not check that the conversation exists in Hermes (see `docs/adr/0001-sqlite-for-sidecar-data.md`): one deleted in Hermes stays referenced until the client takes it out. Deleting it through `DELETE /sessions/{id}` takes it out automatically (see [Sessions](#sessions)).

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

## License

Copyright (C) 2026 Maxence Mottard.

This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed without any warranty. See [LICENSE](LICENSE).
