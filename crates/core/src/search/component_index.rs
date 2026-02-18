//! Component registry indexing and search using SQLite FTS5.

use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::components::cache::get_all_registry_indexes;
use apx_db::dev::{sanitize_fts5_query, table_exists};

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
#[derive(Debug, Clone)]
pub struct ComponentIndex {
    pool: SqlitePool,
}

impl ComponentIndex {
    /// Create a new component index using the provided pool
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create with a specific pool (for testing or custom setups)
    #[allow(dead_code)]
    pub fn with_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Get the FTS table name
    pub fn table_name() -> &'static str {
        TABLE_NAME
    }

    /// Validate that the index exists
    pub async fn validate_index(&self) -> Result<bool, String> {
        table_exists(&self.pool, TABLE_NAME).await
    }

    /// Build index from registry.json files
    pub async fn build_index_from_registries(&self) -> Result<(), String> {
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

        // Drop existing table if it exists
        sqlx::query(&format!("DROP TABLE IF EXISTS {TABLE_NAME}"))
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to drop existing table: {e}"))?;

        // Create FTS5 virtual table
        sqlx::query(&format!(
            "CREATE VIRTUAL TABLE {TABLE_NAME} USING fts5(\
                id UNINDEXED, name, registry UNINDEXED, text, \
                tokenize='porter unicode61'\
            )"
        ))
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create FTS5 table: {e}"))?;

        // Insert all records in a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Transaction error: {e}"))?;

        for record in &records {
            sqlx::query(&format!(
                "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
            ))
            .bind(&record.id)
            .bind(&record.name)
            .bind(&record.registry)
            .bind(&record.text)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Insert error: {e}"))?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Commit error: {e}"))?;

        tracing::info!("Component FTS5 index built successfully");
        Ok(())
    }

    /// Search for components using FTS5
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        tracing::debug!("search: Starting search for query '{}'", query);

        if !table_exists(&self.pool, TABLE_NAME).await? {
            return Err("Index not built. Please ensure components are indexed.".to_string());
        }

        let sanitized = sanitize_fts5_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

        // Fetch more results for reranking
        let fts_limit = (limit * 3).max(30);

        let rows = sqlx::query(&format!(
            "SELECT id, name, registry, rank FROM {TABLE_NAME} \
             WHERE {TABLE_NAME} MATCH ?1 \
             ORDER BY rank \
             LIMIT ?2"
        ))
        .bind(&sanitized)
        .bind(fts_limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Query error: {e}"))?;

        let mut results = Vec::new();

        for (rank, row) in rows.iter().enumerate() {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let registry: String = row.get("registry");

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

    async fn test_index() -> ComponentIndex {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        ComponentIndex::with_pool(pool)
    }

    #[tokio::test]
    async fn test_validate_index_empty() {
        let index = test_index().await;
        assert!(!index.validate_index().await.unwrap());
    }

    #[tokio::test]
    async fn test_search_no_index() {
        let index = test_index().await;
        let result = index.search("button", 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_and_search() {
        let index = test_index().await;

        // Create the FTS table manually and insert test data
        sqlx::query(&format!(
            "CREATE VIRTUAL TABLE {TABLE_NAME} USING fts5(\
                id UNINDEXED, name, registry UNINDEXED, text, \
                tokenize='porter unicode61'\
            )"
        ))
        .execute(&index.pool)
        .await
        .unwrap();

        sqlx::query(&format!(
            "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
        ))
        .bind("button")
        .bind("button")
        .bind("")
        .bind("button A styled button component shadcn")
        .execute(&index.pool)
        .await
        .unwrap();

        sqlx::query(&format!(
            "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
        ))
        .bind("card")
        .bind("card")
        .bind("")
        .bind("card A card container component shadcn")
        .execute(&index.pool)
        .await
        .unwrap();

        sqlx::query(&format!(
            "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
        ))
        .bind("@custom/button")
        .bind("button")
        .bind("custom")
        .bind("button A custom button component")
        .execute(&index.pool)
        .await
        .unwrap();

        let results = index.search("button", 10).await.unwrap();
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

    /// Regression test: multi-term queries should return partial matches instead
    /// of empty results when not all terms appear in a single document.
    /// See: "@animate-ui number counter ticker" returning [] while "animate-ui" alone works.
    #[tokio::test]
    async fn test_multiterm_query_returns_partial_matches() {
        let index = test_index().await;

        sqlx::query(&format!(
            "CREATE VIRTUAL TABLE {TABLE_NAME} USING fts5(\
                id UNINDEXED, name, registry UNINDEXED, text, \
                tokenize='porter unicode61'\
            )"
        ))
        .execute(&index.pool)
        .await
        .unwrap();

        // "animate" in name + "ui component" in text → phrase "animate-ui" matches
        // via FTS5 cross-column phrase matching (animate in name, ui in text).
        // Also has "number", "counter" but NOT "ticker".
        sqlx::query(&format!(
            "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
        ))
        .bind("@animate-ui/number-ticker")
        .bind("animate")
        .bind("animate-ui")
        .bind("animate number counter ui component")
        .execute(&index.pool)
        .await
        .unwrap();

        // Unrelated component
        sqlx::query(&format!(
            "INSERT INTO {TABLE_NAME} (id, name, registry, text) VALUES (?1, ?2, ?3, ?4)"
        ))
        .bind("button")
        .bind("button")
        .bind("")
        .bind("button A styled button component shadcn")
        .execute(&index.pool)
        .await
        .unwrap();

        // Single-term query should find the component
        let results = index.search("animate", 10).await.unwrap();
        assert!(
            results.iter().any(|r| r.id == "@animate-ui/number-ticker"),
            "Single-term 'animate' should match. Got: {results:?}"
        );

        // Multi-term query with a term NOT in the document ("ticker") should
        // still return partial matches, not empty.
        let results = index
            .search("@animate-ui number counter ticker", 10)
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "Multi-term query '@animate-ui number counter ticker' should return partial matches, not empty"
        );
        assert!(
            results.iter().any(|r| r.id == "@animate-ui/number-ticker"),
            "Should find number-ticker component. Got: {results:?}"
        );
    }
}
