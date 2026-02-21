//! Type-safe FTS5 abstraction for SQLite full-text search.
//!
//! Provides [`Fts5Table`] as a builder/handle for FTS5 virtual tables,
//! eliminating raw SQL construction from domain modules.

use sqlx::sqlite::{SqlitePool, SqliteRow};

/// Column definition for an FTS5 table.
#[derive(Debug, Clone)]
pub struct Fts5Column {
    /// Column name (must be alphanumeric + underscore).
    pub name: &'static str,
    /// Whether the column is indexed for full-text search.
    /// `false` maps to FTS5 `UNINDEXED`.
    pub indexed: bool,
}

/// Builder/handle for an FTS5 virtual table.
#[derive(Debug, Clone)]
pub struct Fts5Table {
    pool: SqlitePool,
    table_name: String,
    columns: Vec<Fts5Column>,
    tokenizer: String,
}

impl Fts5Table {
    /// Construct with validated table name (alphanumeric + underscore only).
    pub fn new(
        pool: SqlitePool,
        table_name: &str,
        columns: Vec<Fts5Column>,
    ) -> Result<Self, String> {
        validate_identifier(table_name)?;
        for col in &columns {
            validate_identifier(col.name)?;
        }
        if columns.is_empty() {
            return Err("FTS5 table must have at least one column".to_string());
        }
        Ok(Self {
            pool,
            table_name: table_name.to_string(),
            columns,
            tokenizer: "porter unicode61".to_string(),
        })
    }

    /// Set a custom tokenizer (default: `"porter unicode61"`).
    #[allow(dead_code)]
    pub fn with_tokenizer(mut self, tokenizer: &str) -> Self {
        self.tokenizer = tokenizer.to_string();
        self
    }

    /// Get the table name.
    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    /// Get a reference to the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Check if the table exists in the database.
    pub async fn exists(&self) -> Result<bool, String> {
        super::dev::table_exists(&self.pool, &self.table_name).await
    }

    /// `DROP TABLE IF EXISTS` + `CREATE VIRTUAL TABLE`.
    pub async fn create_or_replace(&self) -> Result<(), String> {
        let drop_sql = format!("DROP TABLE IF EXISTS \"{}\"", self.table_name);
        sqlx::query(&drop_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to drop table '{}': {e}", self.table_name))?;

        let col_defs: Vec<String> = self
            .columns
            .iter()
            .map(|c| {
                if c.indexed {
                    c.name.to_string()
                } else {
                    format!("{} UNINDEXED", c.name)
                }
            })
            .collect();

        let create_sql = format!(
            "CREATE VIRTUAL TABLE \"{}\" USING fts5({}, tokenize='{}')",
            self.table_name,
            col_defs.join(", "),
            self.tokenizer,
        );

        sqlx::query(&create_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to create FTS5 table '{}': {e}", self.table_name))?;

        Ok(())
    }

    /// Begin a transaction on the underlying pool.
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, String> {
        self.pool
            .begin()
            .await
            .map_err(|e| format!("Transaction error: {e}"))
    }

    /// Insert a row of string values into the FTS5 table.
    ///
    /// `values` must have the same length as the number of columns.
    pub async fn insert_str(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        values: &[&str],
    ) -> Result<(), String> {
        if values.len() != self.columns.len() {
            return Err(format!(
                "Expected {} values, got {}",
                self.columns.len(),
                values.len()
            ));
        }

        let col_names: Vec<&str> = self.columns.iter().map(|c| c.name).collect();
        let placeholders: Vec<String> = (1..=values.len()).map(|i| format!("?{i}")).collect();

        let sql = format!(
            "INSERT INTO \"{}\" ({}) VALUES ({})",
            self.table_name,
            col_names.join(", "),
            placeholders.join(", "),
        );

        let mut query = sqlx::query(&sql);
        for val in values {
            query = query.bind(*val);
        }

        query
            .execute(&mut **tx)
            .await
            .map_err(|e| format!("Insert error: {e}"))?;

        Ok(())
    }

    /// FTS5 MATCH search with explicit `bm25()` ranking.
    ///
    /// `bm25_weights` must have one entry per **indexed** column (in column
    /// definition order).  Non-indexed columns are automatically assigned
    /// weight `0.0`.
    pub async fn search_bm25(
        &self,
        match_expr: &str,
        bm25_weights: &[f64],
        limit: usize,
        result_columns: &[&str],
    ) -> Result<Vec<SqliteRow>, String> {
        for col in result_columns {
            if !self.columns.iter().any(|c| c.name == *col) {
                return Err(format!("Unknown column '{col}' in result_columns"));
            }
        }

        let indexed_count = self.columns.iter().filter(|c| c.indexed).count();
        if bm25_weights.len() != indexed_count {
            return Err(format!(
                "Expected {} bm25 weights (one per indexed column), got {}",
                indexed_count,
                bm25_weights.len()
            ));
        }

        // Build full weight array: one per column, 0.0 for unindexed.
        let mut all_weights = Vec::with_capacity(self.columns.len());
        let mut weight_idx = 0;
        for col in &self.columns {
            if col.indexed {
                all_weights.push(bm25_weights[weight_idx]);
                weight_idx += 1;
            } else {
                all_weights.push(0.0);
            }
        }

        let weights_csv: String = all_weights
            .iter()
            .map(|w| format!("{w}"))
            .collect::<Vec<_>>()
            .join(", ");
        let select_cols = result_columns.join(", ");

        let sql = format!(
            "SELECT {select_cols}, bm25(\"{}\", {weights_csv}) AS rank \
             FROM \"{}\" \
             WHERE \"{}\" MATCH ?1 \
             ORDER BY rank \
             LIMIT ?2",
            self.table_name, self.table_name, self.table_name,
        );

        sqlx::query(&sql)
            .bind(match_expr)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Query error: {e}"))
    }

    /// FTS5 MATCH search using the built-in `rank` column.
    pub async fn search(
        &self,
        match_expr: &str,
        limit: usize,
        result_columns: &[&str],
    ) -> Result<Vec<SqliteRow>, String> {
        for col in result_columns {
            if *col != "rank" && !self.columns.iter().any(|c| c.name == *col) {
                return Err(format!("Unknown column '{col}' in result_columns"));
            }
        }

        let select_cols = result_columns.join(", ");

        let sql = format!(
            "SELECT {select_cols} \
             FROM \"{}\" \
             WHERE \"{}\" MATCH ?1 \
             ORDER BY rank \
             LIMIT ?2",
            self.table_name, self.table_name,
        );

        sqlx::query(&sql)
            .bind(match_expr)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Query error: {e}"))
    }
}

// ---------------------------------------------------------------------------
// FTS5 query helpers
// ---------------------------------------------------------------------------

/// Sanitize a query into individually quoted FTS5 terms (no joining).
///
/// Each whitespace-separated token is stripped of non-alphanumeric characters
/// (except `_`, `-`, `.`) and wrapped in double quotes.  Empty tokens are
/// dropped.  The caller is responsible for joining the terms (e.g. with
/// ` OR `).
pub fn sanitize_fts5_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter_map(|term| {
            let clean: String = term
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            if clean.is_empty() {
                None
            } else {
                Some(format!("\"{clean}\""))
            }
        })
        .collect()
}

/// Sanitize a query string for FTS5 MATCH syntax.
///
/// Wraps each whitespace-separated term in double quotes for safe literal
/// matching.  Terms are joined with `OR` so that partial matches are returned —
/// FTS5 ranking naturally scores documents with more matching terms higher.
pub fn sanitize_fts5_query(query: &str) -> String {
    sanitize_fts5_terms(query).join(" OR ")
}

/// Enhance pre-sanitized FTS5 terms to boost entity/class matches.
///
/// Accepts individually quoted terms (e.g. `['"GenieAttachment"', '"fields"']`)
/// and returns a single FTS5 MATCH expression joined with `OR`.
///
/// - If a term is PascalCase, an extra `entity:<term>` clause is added.
/// - Hint words like "fields" / "attributes" are stripped (they only guide
///   boosting).
pub fn enhance_fts5_query(terms: &[String]) -> String {
    let hint_words: &[&str] = &["fields", "attributes", "members", "properties"];

    let mut entity_terms = Vec::new();
    let mut regular_terms = Vec::new();

    for token in terms {
        let clean = token.trim_matches('"');
        if hint_words.contains(&clean.to_lowercase().as_str()) {
            continue;
        }
        if is_pascal_case(clean) {
            entity_terms.push(format!("entity:{token}"));
            regular_terms.push(token.clone());
        } else {
            regular_terms.push(token.clone());
        }
    }

    let mut parts = entity_terms;
    parts.extend(regular_terms);
    parts.join(" OR ")
}

/// Check if a string looks like PascalCase (starts with uppercase, contains lowercase).
pub fn is_pascal_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    // Must contain at least one lowercase letter (to distinguish from ALL_CAPS)
    s.chars().any(|c| c.is_ascii_lowercase())
}

/// Validate that an identifier contains only alphanumeric characters and underscores.
fn validate_identifier(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Identifier cannot be empty".to_string());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(format!(
            "Invalid identifier '{name}': only alphanumeric characters and underscores allowed"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // validate_identifier
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_identifier_valid() {
        assert!(validate_identifier("my_table_v1").is_ok());
        assert!(validate_identifier("abc").is_ok());
        assert!(validate_identifier("A123").is_ok());
    }

    #[test]
    fn test_validate_identifier_invalid() {
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("my-table").is_err());
        assert!(validate_identifier("my table").is_err());
        assert!(validate_identifier("table;DROP").is_err());
    }

    // -----------------------------------------------------------------------
    // sanitize_fts5_terms / sanitize_fts5_query
    // -----------------------------------------------------------------------

    #[test]
    fn test_sanitize_fts5_terms() {
        assert_eq!(
            sanitize_fts5_terms("serving endpoints"),
            vec!["\"serving\"", "\"endpoints\""]
        );
        assert_eq!(
            sanitize_fts5_terms("hello* OR world"),
            vec!["\"hello\"", "\"OR\"", "\"world\""]
        );
        assert!(sanitize_fts5_terms("").is_empty());
        assert!(sanitize_fts5_terms("   ").is_empty());
    }

    #[test]
    fn test_sanitize_fts5_query_basic() {
        assert_eq!(sanitize_fts5_query("hello world"), "\"hello\" OR \"world\"");
    }

    #[test]
    fn test_sanitize_fts5_query_special_chars() {
        assert_eq!(
            sanitize_fts5_query("hello* OR world"),
            "\"hello\" OR \"OR\" OR \"world\""
        );
    }

    #[test]
    fn test_sanitize_fts5_query_empty() {
        assert_eq!(sanitize_fts5_query(""), "");
        assert_eq!(sanitize_fts5_query("   "), "");
    }

    // -----------------------------------------------------------------------
    // is_pascal_case
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_pascal_case() {
        assert!(is_pascal_case("GenieAttachment"));
        assert!(is_pascal_case("ClustersAPI"));
        assert!(is_pascal_case("Abc"));
        assert!(!is_pascal_case("abc"));
        assert!(!is_pascal_case("ABC")); // ALL_CAPS, not PascalCase
        assert!(!is_pascal_case("create"));
        assert!(!is_pascal_case("123"));
    }

    // -----------------------------------------------------------------------
    // enhance_fts5_query
    // -----------------------------------------------------------------------

    #[test]
    fn test_enhance_fts5_query_pascal_case() {
        let terms = vec!["\"GenieAttachment\"".to_string()];
        let result = enhance_fts5_query(&terms);
        assert!(result.contains("entity:\"GenieAttachment\""));
        assert!(result.contains("\"GenieAttachment\""));
    }

    #[test]
    fn test_enhance_fts5_query_with_fields_hint() {
        let terms = vec!["\"GenieAttachment\"".to_string(), "\"fields\"".to_string()];
        let result = enhance_fts5_query(&terms);
        assert!(result.contains("entity:\"GenieAttachment\""));
        // "fields" is a hint word and should be stripped
        assert!(!result.contains("\"fields\""));
    }

    #[test]
    fn test_enhance_fts5_query_plain() {
        let terms = vec!["\"create\"".to_string(), "\"clusters\"".to_string()];
        let result = enhance_fts5_query(&terms);
        assert_eq!(result, "\"create\" OR \"clusters\"");
    }

    /// Regression: multi-word queries must not produce `OR OR`.
    #[test]
    fn test_enhance_fts5_query_no_double_or() {
        let terms = vec!["\"serving\"".to_string(), "\"endpoints\"".to_string()];
        let result = enhance_fts5_query(&terms);
        assert!(
            !result.contains("OR OR"),
            "must not contain double OR: {result}"
        );
        assert_eq!(result, "\"serving\" OR \"endpoints\"");
    }

    // -----------------------------------------------------------------------
    // Fts5Table: construction
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_fts5_table_new_valid() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let table = Fts5Table::new(
            pool,
            "my_table_v1",
            vec![
                Fts5Column {
                    name: "id",
                    indexed: false,
                },
                Fts5Column {
                    name: "text",
                    indexed: true,
                },
            ],
        );
        assert!(table.is_ok());
    }

    #[tokio::test]
    async fn test_fts5_table_rejects_bad_table_name() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let table = Fts5Table::new(
            pool,
            "my-table",
            vec![Fts5Column {
                name: "text",
                indexed: true,
            }],
        );
        assert!(table.is_err());
    }

    #[tokio::test]
    async fn test_fts5_table_rejects_empty_columns() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let table = Fts5Table::new(pool, "t", vec![]);
        assert!(table.is_err());
    }

    // -----------------------------------------------------------------------
    // Fts5Table: round-trip (create → insert → search)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_fts5_round_trip() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let fts = Fts5Table::new(
            pool,
            "test_rt_v1",
            vec![
                Fts5Column {
                    name: "id",
                    indexed: false,
                },
                Fts5Column {
                    name: "body",
                    indexed: true,
                },
                Fts5Column {
                    name: "tag",
                    indexed: true,
                },
            ],
        )
        .unwrap();

        fts.create_or_replace().await.unwrap();
        assert!(fts.exists().await.unwrap());

        let mut tx = fts.begin().await.unwrap();
        fts.insert_str(&mut tx, &["1", "hello world", "greeting"])
            .await
            .unwrap();
        fts.insert_str(&mut tx, &["2", "goodbye world", "farewell"])
            .await
            .unwrap();
        tx.commit()
            .await
            .map_err(|e| format!("commit: {e}"))
            .unwrap();

        // search with default rank
        let rows = fts
            .search("\"hello\"", 10, &["id", "body", "rank"])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);

        // search_bm25 with custom weights (body=1.0, tag=2.0)
        let rows = fts
            .search_bm25("\"world\"", &[1.0, 2.0], 10, &["id", "body"])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn test_fts5_insert_str_wrong_count() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let fts = Fts5Table::new(
            pool,
            "test_cnt_v1",
            vec![
                Fts5Column {
                    name: "a",
                    indexed: true,
                },
                Fts5Column {
                    name: "b",
                    indexed: true,
                },
            ],
        )
        .unwrap();
        fts.create_or_replace().await.unwrap();

        let mut tx = fts.begin().await.unwrap();
        let result = fts.insert_str(&mut tx, &["only_one"]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fts5_search_bm25_wrong_weight_count() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let fts = Fts5Table::new(
            pool,
            "test_bm_v1",
            vec![
                Fts5Column {
                    name: "id",
                    indexed: false,
                },
                Fts5Column {
                    name: "body",
                    indexed: true,
                },
            ],
        )
        .unwrap();
        fts.create_or_replace().await.unwrap();

        // Two weights for one indexed column → error
        let result = fts.search_bm25("\"hello\"", &[1.0, 2.0], 10, &["id"]).await;
        assert!(result.is_err());
    }
}
