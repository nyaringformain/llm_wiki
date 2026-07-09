CREATE TABLE project_registry (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    relative_path TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL CHECK (source IN ('created', 'imported')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_opened_at TEXT
);

CREATE INDEX idx_project_registry_name
    ON project_registry(name);
