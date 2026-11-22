-- Projects group Conversations (Hermes sessions) and git Repositories. See GLOSSARY.md.

CREATE TABLE projects (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    -- Lowercased name, set by the sidecar: names are unique ignoring case, Unicode included,
    -- which SQLite's NOCASE collation (ASCII only) cannot enforce.
    name_key TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE project_repositories (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- Normalised remote URL, e.g. https://github.com/owner/name.
    url TEXT NOT NULL,
    -- Path after the host, derived from the URL, e.g. owner/name.
    name TEXT NOT NULL,
    UNIQUE (project_id, url)
) STRICT;

-- A Conversation is in at most one Project, hence the primary key on its id alone.
CREATE TABLE project_conversations (
    conversation_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE
) STRICT;

CREATE INDEX project_conversations_project_id ON project_conversations (project_id);
