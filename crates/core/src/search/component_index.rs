//! Component registry indexing and search using SQLite FTS5.

use rusqlite::Connection;
use std::sync::{Arc, Mutex};

use super::common;
use crate::components::cache::get_all_registry_indexes;

const TABLE_NAME: &str = "components_fts_v1";

/// Component record for FTS storage
#[derive(Debug, Clone)]
pub struct ComponentRecord {
    /// Component ID: either "component-name" or "@registry-name/component-name"
    pub id: String,
    /// Component name
    pub name: String,
    /// Registry name (empty for default shadcn/ui)
    pub registry: String,
    /// Full searchable text (name + description)
    pub text: String,
}

/// Search result with component details
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub name: String,
    pub registry: String,
    pub score: f32,
}

/// Component search index using SQLite FTS5
#[derive(Debug)]
pub struct ComponentIndex {
    conn: Arc<Mutex<Connection>>,
}

impl ComponentIndex {
    /// Create a new component index using the global search database
    pub fn new() -> Result<Self, String> {
        let conn = common::get_connection()?;
        Ok(Self { conn })
    }

    /// Create with a custom connection (for testing)
    #[allow(dead_code)]
    pub fn with_connection(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Get the FTS table name
    pub fn table_name() -> &'static str {
        TABLE_NAME
    }

    /// Validate that the index exists
    pub fn validate_index(&self) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {e}"))?;
        common::table_exists(&conn, TABLE_NAME)
    }

    /// Build index from registry.json files
    pub fn build_index_from_registries(&self) -> Result<(), String> {
        tracing::info!("Building component FTS5 index from registry indexes");

        let all_indexes = get_all_registry_indexes()
            .map_err(|e| format!("Failed to load registry indexes: {e}"))?;

        if all_indexes.is_empty() {
            tracing::warn!("No registry indexes found. Index will be empty.");
            return Ok(());
        }

        let mut records: Vec<ComponentRecord> = Vec::new();

        for (registry_name, items) in all_indexes {
            let is_default = registry_name == "ui";

            for item in items {
                let (id, registry) = if is_default {
                    (item.name.clone(), String::new())
                } else {
                    (
                        format!("@{}/{}", registry_name, item.name),
                        registry_name.clone(),
                    )
                };

                let text = match &item.description {
                    Some(desc) if !desc.is_empty() => {
                        format!("{} {} ui component shadcn", item.name, desc)
                    }
                    _ => format!("{} ui component shadcn", item.name),
                };

                records.push(ComponentRecord {
                    id,
                    name: item.name,
                    registry,
                    text,
                });
            }
        }

        tracing::info!("Indexing {} components", records.len());

        let conn = self.conn.lock().map_err(|e| format!("Lock error: {e}"))?;

        // Drop existing table if it exists
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {TABLE_NAME}"))
            .map_err(|e| format!("Failed to drop existing table: {e}"))?;

        // Create FTS5 virtual table
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE {TABLE_NAME} USING fts5(\
                id UNINDEXED, name, registry UNINDEXED, text, \
                tokenize='porter unicode61'\
            )"
        ))
        .map_err(|e| format!("Failed to create FTS5 table: {e}"))?;

        // Insert all records in a transaction
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction error: {e}"))?;

        {
            let mut stmt = tx
                .prepare(&format!(
                    "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
                ))
                .map_err(|e| format!("Prepare error: {e}"))?;

            for record in &records {
                stmt.execute(rusqlite::params![
                    record.id,
                    record.name,
                    record.registry,
                    record.text,
                ])
                .map_err(|e| format!("Insert error: {e}"))?;
            }
        }

        tx.commit().map_err(|e| format!("Commit error: {e}"))?;

        tracing::info!("Component FTS5 index built successfully");
        Ok(())
    }

    /// Search for components using FTS5
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        tracing::debug!("search: Starting search for query '{}'", query);

        let conn = self.conn.lock().map_err(|e| format!("Lock error: {e}"))?;

        if !common::table_exists(&conn, TABLE_NAME)? {
            return Err("Index not built. Please ensure components are indexed.".to_string());
        }

        let sanitized = common::sanitize_fts5_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

        // Fetch more results for reranking
        let fts_limit = (limit * 3).max(30);

        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, name, registry, rank FROM {TABLE_NAME} \
                 WHERE {TABLE_NAME} MATCH ?1 \
                 ORDER BY rank \
                 LIMIT ?2"
            ))
            .map_err(|e| format!("Prepare error: {e}"))?;

        let rows = stmt
            .query_map(rusqlite::params![sanitized, fts_limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })
            .map_err(|e| format!("Query error: {e}"))?;

        let mut results = Vec::new();

        for (rank, row_result) in rows.enumerate() {
            let (id, name, registry, _fts_rank) =
                row_result.map_err(|e| format!("Row error: {e}"))?;

            let name_lower = name.to_lowercase();

            // Base score from result position (higher rank = lower base score)
            let base_score = 1.0 / (1.0 + rank as f32);

            // Registry boost: strongly prefer default (shadcn) components
            let registry_boost = if registry.is_empty() { 0.5 } else { 0.0 };

            // Name match boost
            let name_match_boost = if query_terms.iter().any(|term| *term == name_lower) {
                2.0
            } else if query_terms
                .iter()
                .any(|term| name_lower.contains(term) || term.contains(&name_lower))
            {
                0.3
            } else {
                0.0
            };

            let score = base_score + registry_boost + name_match_boost;

            results.push(SearchResult {
                id,
                name,
                registry,
                score,
            });
        }

        // Re-sort by score after applying boosts
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results.truncate(limit);

        tracing::info!("FTS5 search for '{}': {} results", query, results.len());

        Ok(results)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn test_index() -> ComponentIndex {
        let conn = Connection::open_in_memory().unwrap();
        let conn = Arc::new(Mutex::new(conn));
        ComponentIndex::with_connection(conn)
    }

    #[test]
    fn test_validate_index_empty() {
        let index = test_index();
        assert!(!index.validate_index().unwrap());
    }

    #[test]
    fn test_search_no_index() {
        let index = test_index();
        let result = index.search("button", 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_and_search() {
        let index = test_index();

        // Create the FTS table manually and insert test data
        {
            let conn = index.conn.lock().unwrap();
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE {TABLE_NAME} USING fts5(\
                    id UNINDEXED, name, registry UNINDEXED, text, \
                    tokenize='porter unicode61'\
                )"
            ))
            .unwrap();

            conn.execute(
                &format!(
                    "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
                ),
                rusqlite::params![
                    "button",
                    "button",
                    "",
                    "button A styled button component shadcn"
                ],
            )
            .unwrap();

            conn.execute(
                &format!(
                    "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
                ),
                rusqlite::params!["card", "card", "", "card A card container component shadcn"],
            )
            .unwrap();

            conn.execute(
                &format!(
                    "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
                ),
                rusqlite::params![
                    "@custom/button",
                    "button",
                    "custom",
                    "button A custom button component"
                ],
            )
            .unwrap();
        }

        let results = index.search("button", 10).unwrap();
        assert!(!results.is_empty());

        // Both buttons should be in results, card should not
        let button_results: Vec<_> = results.iter().filter(|r| r.name == "button").collect();
        assert_eq!(button_results.len(), 2);

        // Default registry button should have higher score than custom
        let default_btn = results.iter().find(|r| r.id == "button").unwrap();
        let custom_btn = results.iter().find(|r| r.id == "@custom/button").unwrap();
        assert!(
            default_btn.score >= custom_btn.score,
            "Default registry button (score={}) should rank >= custom (score={})",
            default_btn.score,
            custom_btn.score
        );
    }
}
