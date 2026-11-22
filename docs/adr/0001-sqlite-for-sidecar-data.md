# SQLite file owned by the sidecar for its own data

Projects need persistent storage, and until now the sidecar had none. We store its data in a SQLite file of its own (`DATABASE_PATH`, on a dedicated volume), accessed with `sqlx` and migrated at startup. The data is small and has a single user (one API key), so a separate database server would add a container, credentials and backups for no gain.

## Considered Options

- **Postgres as its own compose service**: rejected; one more service to run, secure and back up, for a few hundred rows.
- **Tables in Hermes's `state.db`**: rejected; it is Hermes's schema, which Hermes migrates itself, and the sidecar must not write into Hermes's data (it only mounts the image store, read-only).

## Consequences

The sidecar references Conversations by their Hermes session id only and does not check that they exist: the Hermes API server has no route to read sessions, and mounting `state.db` would expose every message to the sidecar. A Conversation deleted in Hermes stays referenced until the client detaches it.
