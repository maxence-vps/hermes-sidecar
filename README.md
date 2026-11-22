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
| `RUST_LOG` | no | `info` | Log filter. |

## Endpoints

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/health` | no | Liveness probe. Does not call Hermes. |
| `GET` | `/api/status` | yes | Checks the key and that Hermes is reachable. `502` if Hermes is down. |

Errors are JSON: `{"error": {"code": "...", "message": "..."}}`.

Add new endpoints in `src/routes.rs`, in the `protected` router, and call Hermes through `HermesClient` (`src/hermes.rs`).

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
