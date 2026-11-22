# hermes-sidecar

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
| `BIND_ADDR` | no | `0.0.0.0:8080` | Listen address. |
| `API_DOCS_ENABLED` | no | `false` | Serves the OpenAPI spec and the Scalar docs (`true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off`). Any other value stops startup. |
| `RUST_LOG` | no | `info` | Log filter. |

## Endpoints

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/health` | no | Liveness probe. Does not call Hermes. |
| `GET` | `/status` | yes | Checks the key and that Hermes is reachable. `502` if Hermes is down. |
| `GET` | `/openapi.json` | no | OpenAPI spec. Only when `API_DOCS_ENABLED=true`. |
| `GET` | `/docs` | no | [Scalar](https://scalar.com) API reference. Only when `API_DOCS_ENABLED=true`. |

Behind the reverse proxy the sidecar is served under a path prefix (for example `https://hermes-api.example.test/sidecar/status`); the proxy strips the prefix, so routes here have none.

Errors are JSON: `{"error": {"code": "...", "message": "..."}}`.

## OpenAPI docs

The spec is generated at compile time with [utoipa](https://github.com/juhaku/utoipa) from the `#[utoipa::path]` annotations on the handlers, so a route registered with `routes!(...)` is always documented. The Scalar page embeds the spec and loads Scalar itself from a CDN, so the binary barely grows.

Behind a proxy that strips a path prefix, the spec's server URL is taken from the `X-Forwarded-Prefix` header (set by Traefik's strip-prefix middleware), so "Try it" calls `/sidecar/status` and not `/status`.

## Adding an endpoint

Write the handler in `src/routes.rs` with a `#[utoipa::path(...)]` annotation (add `security(("bearer" = []))` for protected routes) and `ToSchema` response types, then register it in the `protected` router with `routes!(...)`. Call Hermes through `HermesClient` (`src/hermes.rs`).

## Development

Tools are pinned with [mise](https://mise.jdx.dev).

```sh
mise install          # Rust toolchain
mise run test         # tests
mise run lint         # rustfmt + clippy
API_SERVER_KEY=dev-key HERMES_URL=http://localhost:8642 mise run run
```

## Docker

The image is a static musl binary in an empty (`scratch`) image, with no TLS library: the sidecar only talks to Hermes in plain HTTP on the internal network, and HTTPS is handled by the reverse proxy.

`.github/workflows/build.yml` builds the image and pushes it to `ghcr.io/maxence-vps/hermes-sidecar` on every push to `main`, tagged `latest` and `sha-<commit>`. Pin a `sha-` tag in production.

```sh
docker build -t hermes-sidecar .
```

See `docker-compose.example.yml` for running it next to Hermes.
