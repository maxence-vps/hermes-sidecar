//! The sidecar's own SQLite database (`DATABASE_PATH`). See `docs/adr/0001-sqlite-for-sidecar-data.md`.

use std::{path::Path, str::FromStr as _, time::Duration};

use sqlx::{
    SqlitePool,
    migrate::MigrateError,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

/// How long a write waits for another one to release the database lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("cannot open the database: {0}")]
    Open(#[source] sqlx::Error),
    #[error("cannot apply the database migrations: {0}")]
    Migrate(#[source] MigrateError),
}

/// Opens the database at `path`, creating the file if missing, then applies the pending
/// migrations embedded in the binary.
pub async fn open(path: &Path) -> Result<SqlitePool, DbError> {
    // `filename` takes the path as is, without parsing it as a URL.
    let options = SqliteConnectOptions::from_str("sqlite:")
        .map_err(DbError::Open)?
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT);
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .map_err(DbError::Open)?;
    sqlx::migrate!()
        .run(&pool)
        .await
        .map_err(DbError::Migrate)?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn tables(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_%' ESCAPE '\\' \
             AND name NOT LIKE 'sqlite%' ORDER BY name",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn creates_the_file_and_the_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sidecar.db");
        let pool = open(&path).await.unwrap();
        assert!(path.exists());
        assert_eq!(
            tables(&pool).await,
            ["project_conversations", "project_repositories", "projects"]
        );
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[tokio::test]
    async fn keeps_the_data_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sidecar.db");

        let pool = open(&path).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, name_key) VALUES ('p1', 'Hermes', 'hermes')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let pool = open(&path).await.unwrap();
        let name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = 'p1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Hermes");
    }

    #[tokio::test]
    async fn enforces_the_constraints() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open(&dir.path().join("sidecar.db")).await.unwrap();
        let run = |sql: &'static str| {
            let pool = pool.clone();
            async move { sqlx::query(sql).execute(&pool).await }
        };

        run("INSERT INTO projects (id, name, name_key) VALUES ('p1', 'Hermes', 'hermes')")
            .await
            .unwrap();
        run("INSERT INTO projects (id, name, name_key) VALUES ('p2', 'Infra', 'infra')")
            .await
            .unwrap();
        // Same name ignoring case.
        run("INSERT INTO projects (id, name, name_key) VALUES ('p3', 'HERMES', 'hermes')")
            .await
            .unwrap_err();

        run(concat!(
            "INSERT INTO project_repositories (id, project_id, url, name) ",
            "VALUES ('r1', 'p1', 'https://github.com/o/n', 'o/n')"
        ))
        .await
        .unwrap();
        // Same URL twice in a project, but allowed in another one.
        run(concat!(
            "INSERT INTO project_repositories (id, project_id, url, name) ",
            "VALUES ('r2', 'p1', 'https://github.com/o/n', 'o/n')"
        ))
        .await
        .unwrap_err();
        run(concat!(
            "INSERT INTO project_repositories (id, project_id, url, name) ",
            "VALUES ('r3', 'p2', 'https://github.com/o/n', 'o/n')"
        ))
        .await
        .unwrap();
        // Unknown project.
        run(concat!(
            "INSERT INTO project_repositories (id, project_id, url, name) ",
            "VALUES ('r4', 'nope', 'https://github.com/o/m', 'o/m')"
        ))
        .await
        .unwrap_err();

        run("INSERT INTO project_conversations (conversation_id, project_id) VALUES ('c1', 'p1')")
            .await
            .unwrap();
        // A conversation is in at most one project.
        run("INSERT INTO project_conversations (conversation_id, project_id) VALUES ('c1', 'p2')")
            .await
            .unwrap_err();

        // Deleting a project removes its links.
        run("DELETE FROM projects WHERE id = 'p1'").await.unwrap();
        let left: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM project_repositories WHERE project_id = 'p1') \
             + (SELECT count(*) FROM project_conversations WHERE project_id = 'p1')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 0);
    }
}
