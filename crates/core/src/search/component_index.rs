//! Component registry indexing and search using SQLite FTS5.

use sqlx::Row;
use sqlx::sqlite::SqlitePool;
use std::collections::HashSet;

use crate::components::cache::get_all_registry_indexes;
use apx_db::fts::{Fts5Column, Fts5Table, sanitize_fts5_query};

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
    /// Unique component identifier.
    pub id: String,
    /// Human-readable component name.
    pub name: String,
    /// Registry the component belongs to.
    pub registry: String,
    /// FTS5 relevance score (lower is more relevant).
    pub score: f32,
}

/// FTS5 column layout for component tables.
fn component_fts_columns() -> Vec<Fts5Column> {
    vec![
        Fts5Column {
            name: "id",
            indexed: false,
        },
        Fts5Column {
            name: "name",
            indexed: true,
        },
        Fts5Column {
            name: "registry",
            indexed: false,
        },
        Fts5Column {
            name: "text",
            indexed: true,
        },
    ]
}

/// Component search index using SQLite FTS5
#[derive(Debug, Clone)]
pub struct ComponentIndex {
    fts: Fts5Table,
}

impl ComponentIndex {
    /// Create a new component index using the provided pool
    pub fn new(pool: SqlitePool) -> Result<Self, String> {
        let fts = Fts5Table::new(pool, TABLE_NAME, component_fts_columns())?;
        Ok(Self { fts })
    }

    /// Get the FTS table name
    pub fn table_name() -> &'static str {
        TABLE_NAME
    }

    /// Validate that the index exists
    pub async fn validate_index(&self) -> Result<bool, String> {
        self.fts.exists().await
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

        self.fts.create_or_replace().await?;

        // Insert all records in a transaction
        let mut tx = self.fts.begin().await?;

        for record in &records {
            self.fts
                .insert_str(
                    &mut tx,
                    &[&record.id, &record.name, &record.registry, &record.text],
                )
                .await?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Commit error: {e}"))?;

        tracing::info!("Component FTS5 index built successfully");
        Ok(())
    }

    /// Search for components using FTS5.
    /// When `configured_registries` is provided, components from those registries
    /// receive a scoring boost to improve discoverability of project-relevant components.
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        configured_registries: Option<&HashSet<String>>,
    ) -> Result<Vec<SearchResult>, String> {
        tracing::debug!("search: Starting search for query '{}'", query);

        if !self.fts.exists().await? {
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

        let rows = self
            .fts
            .search(&sanitized, fts_limit, &["id", "name", "registry", "rank"])
            .await?;

        let mut results = Vec::new();

        for (rank, row) in rows.iter().enumerate() {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let registry: String = row.get("registry");

            let name_lower = name.to_lowercase();

            // Base score from result position (higher rank = lower base score)
            let base_score = 1.0 / (1.0 + rank as f32);

            // Registry boost: prefer project-configured registries, then default
            let registry_boost = if registry.is_empty() {
                0.5 // default shadcn/ui
            } else if configured_registries.is_some_and(|cr| cr.contains(&registry)) {
                1.0 // project-configured custom registry
            } else {
                0.0 // unconfigured custom registry
            };

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
// Reason: panicking on failure is idiomatic in tests
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    impl ComponentIndex {
        /// Create with a specific pool (for testing)
        pub fn with_pool(pool: SqlitePool) -> Result<Self, String> {
            Self::new(pool)
        }
    }

    async fn test_index() -> ComponentIndex {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        ComponentIndex::with_pool(pool).unwrap()
    }

    /// Helper: create the FTS table and insert test data via the Fts5Table API.
    async fn seed_test_data(fts: &Fts5Table, rows: &[(&str, &str, &str, &str)]) {
        fts.create_or_replace().await.unwrap();
        let mut tx = fts.begin().await.unwrap();
        for (id, name, registry, text) in rows {
            fts.insert_str(&mut tx, &[*id, *name, *registry, *text])
                .await
                .unwrap();
        }
        tx.commit().await.map_err(|e| format!("{e}")).unwrap();
    }

    #[tokio::test]
    async fn test_validate_index_empty() {
        let index = test_index().await;
        assert!(!index.validate_index().await.unwrap());
    }

    #[tokio::test]
    async fn test_search_no_index() {
        let index = test_index().await;
        let result = index.search("button", 10, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_and_search() {
        let index = test_index().await;

        seed_test_data(
            &index.fts,
            &[
                (
                    "button",
                    "button",
                    "",
                    "button A styled button component shadcn",
                ),
                ("card", "card", "", "card A card container component shadcn"),
                (
                    "@custom/button",
                    "button",
                    "custom",
                    "button A custom button component",
                ),
            ],
        )
        .await;

        let results = index.search("button", 10, None).await.unwrap();
        assert!(!results.is_empty());

        // Both buttons should be in results, card should not
        assert_eq!(results.iter().filter(|r| r.name == "button").count(), 2);

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

        seed_test_data(
            &index.fts,
            &[
                (
                    "@animate-ui/number-ticker",
                    "animate",
                    "animate-ui",
                    "animate number counter ui component",
                ),
                (
                    "button",
                    "button",
                    "",
                    "button A styled button component shadcn",
                ),
            ],
        )
        .await;

        // Single-term query should find the component
        let results = index.search("animate", 10, None).await.unwrap();
        assert!(
            results.iter().any(|r| r.id == "@animate-ui/number-ticker"),
            "Single-term 'animate' should match. Got: {results:?}"
        );

        // Multi-term query with a term NOT in the document ("ticker") should
        // still return partial matches, not empty.
        let results = index
            .search("@animate-ui number counter ticker", 10, None)
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

    #[tokio::test]
    async fn test_configured_registry_boost() {
        let index = test_index().await;

        seed_test_data(
            &index.fts,
            &[
                (
                    "textarea",
                    "textarea",
                    "",
                    "textarea A text input area component shadcn",
                ),
                (
                    "@ai-elements/message",
                    "message",
                    "ai-elements",
                    "message chat message bubble ai component",
                ),
            ],
        )
        .await;

        // Without configured registries: default gets boosted
        let results = index.search("message", 10, None).await.unwrap();
        let ai_msg = results.iter().find(|r| r.id == "@ai-elements/message");
        assert!(ai_msg.is_some(), "Should find ai-elements message");

        // With configured registries: ai-elements gets 1.0 boost
        let configured: HashSet<String> = ["ai-elements".to_string()].into();
        let results = index
            .search("message", 10, Some(&configured))
            .await
            .unwrap();
        let ai_msg = results
            .iter()
            .find(|r| r.id == "@ai-elements/message")
            .unwrap();
        assert!(
            ai_msg.score >= 1.0,
            "Configured registry component should have score >= 1.0, got {}",
            ai_msg.score
        );
    }
}
