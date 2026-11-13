//! # Tool search (BM25)
//!
//! `search_tools` runs a full-text search over the tools of every upstream.
//! We rely on the [`bm25`] crate (pure Rust, lightweight) rather than a full engine
//! (tantivy, DuckDB): there are at most a few hundred tools, the index is rebuilt in a few
//! milliseconds and fits in memory.
//!
//! ## What makes search good
//!
//! Quality mostly depends on the **document** indexed for each tool ([`tool_document`]):
//!
//! - the tool name, **split** (`search_melodi_datasets` → "search melodi datasets"),
//!   repeated twice to give it more weight (the "poor man's boost" in BM25);
//! - the **server's** name, domain, tags and description: a search on a domain
//!   ("diffusion") thus finds the tools of the matching server;
//! - the tool's description;
//! - the names and descriptions of its **parameters** (`geo_level`, `dataset_id`…).
//!
//! The tokenizer is tuned for French, the language of the current upstreams: accents
//! removed, stop words ("le", "des"…) and stemming ("publications" and "publication" match).
//!
//! ## Why a trait?
//!
//! [`ToolIndex`] isolates search from the rest of the gateway. This module depends only on
//! `rmcp::model::Tool`: it can be extracted as is (separate crate or PR to rmcp).

use bm25::{Document, Language, LanguageMode, SearchEngine, SearchEngineBuilder};
use rmcp::model::Tool;

/// A search result: the tool's exposed name and its BM25 score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredTool {
    pub name: String,
    pub score: f32,
}

/// Minimal interface of a tool search index.
pub trait ToolIndex: Send + Sync {
    /// Returns at most `limit` tools, from most to least relevant.
    fn search(&self, query: &str, limit: usize) -> Vec<ScoredTool>;
}

/// What we know about a tool when indexing it.
pub struct IndexEntry<'a> {
    /// Name exposed by the gateway (`{server}_{tool}`), used as the identifier.
    pub exposed_name: &'a str,
    pub tool: &'a Tool,
    /// Text describing the server (name, domain, tags, description).
    pub server_text: &'a str,
}

/// BM25 index built once and for all (immutable) from a list of tools.
pub struct Bm25ToolIndex {
    /// Document identifiers are indices into `names` (an integer is more efficient than a
    /// string for the bm25 crate).
    engine: SearchEngine<usize>,
    names: Vec<String>,
}

impl Bm25ToolIndex {
    pub fn build<'a>(entries: impl IntoIterator<Item = IndexEntry<'a>>) -> Self {
        let mut names = Vec::new();
        let mut documents = Vec::new();
        for (id, entry) in entries.into_iter().enumerate() {
            documents.push(Document::new(id, tool_document(&entry)));
            names.push(entry.exposed_name.to_owned());
        }
        let engine = SearchEngineBuilder::<usize>::with_documents(
            LanguageMode::Fixed(Language::French),
            documents,
        )
        .build();
        Self { engine, names }
    }
}

impl ToolIndex for Bm25ToolIndex {
    fn search(&self, query: &str, limit: usize) -> Vec<ScoredTool> {
        // The query goes through the same preprocessing as the documents (splitting on `_`).
        self.engine
            .search(&split_identifiers(query), limit)
            .into_iter()
            .map(|r| ScoredTool {
                name: self.names[r.document.id].clone(),
                score: r.score,
            })
            .collect()
    }
}

/// Builds the indexed text for a tool. See the module documentation.
pub fn tool_document(entry: &IndexEntry<'_>) -> String {
    let tool = entry.tool;
    let mut parts: Vec<String> = Vec::new();

    // Exposed name, twice (boost): it contains the server prefix and the upstream name.
    parts.push(entry.exposed_name.to_owned());
    parts.push(entry.exposed_name.to_owned());
    if let Some(title) = &tool.title {
        parts.push(title.clone());
    }
    parts.push(entry.server_text.to_owned());
    if let Some(description) = &tool.description {
        parts.push(description.to_string());
    }

    // Parameters: `inputSchema.properties.{name}.description`.
    if let Some(serde_json::Value::Object(props)) = tool.input_schema.get("properties") {
        for (param, schema) in props {
            parts.push(param.clone());
            if let Some(desc) = schema.get("description").and_then(|d| d.as_str()) {
                parts.push(desc.to_owned());
            }
        }
    }

    split_identifiers(&parts.join("\n"))
}

/// Replaces `_` and `-` with spaces: Unicode word segmentation treats
/// `search_melodi_datasets` as **a single** word, which would make "melodi" unfindable.
fn split_identifiers(text: &str) -> String {
    text.replace(['_', '-'], " ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    fn tool(name: &str, description: &str, schema: serde_json::Value) -> Tool {
        let schema = schema.as_object().cloned().unwrap_or_default();
        Tool::new(name.to_owned(), description.to_owned(), Arc::new(schema))
    }

    // French test data on purpose: it exercises the French tokenizer.
    fn index() -> Bm25ToolIndex {
        let geocode = tool(
            "geocode",
            "Géocode une adresse ou un lieu et renvoie ses coordonnées.",
            json!({"type": "object", "properties": {"text": {"type": "string", "description": "Adresse à géocoder"}}}),
        );
        let datasets = tool(
            "search_melodi_datasets",
            "Recherche des jeux de données statistiques dans le catalogue MELODI.",
            json!({"type": "object", "properties": {"query": {"type": "string"}, "start_year": {"type": "integer", "description": "Première année"}}}),
        );
        let cadastre = tool(
            "cadastre",
            "Renvoie les parcelles cadastrales à une position.",
            json!({"type": "object", "properties": {"lat": {"type": "number"}, "lon": {"type": "number"}}}),
        );
        Bm25ToolIndex::build([
            IndexEntry {
                exposed_name: "geodata_geocode",
                tool: &geocode,
                server_text: "geodata géographie IGN",
            },
            IndexEntry {
                exposed_name: "melodi_search_melodi_datasets",
                tool: &datasets,
                server_text: "melodi Insee dataset",
            },
            IndexEntry {
                exposed_name: "geodata_cadastre",
                tool: &cadastre,
                server_text: "geodata géographie IGN",
            },
        ])
    }

    #[test]
    fn finds_tool_by_word_inside_identifier() {
        // "melodi" only appears in identifiers with `_`: splitting is essential.
        let hits = index().search("melodi", 5);
        assert_eq!(hits[0].name, "melodi_search_melodi_datasets");
    }

    #[test]
    fn ignores_accents_and_plural() {
        // "donnees" without accent and in the plural must find "données".
        let hits = index().search("jeu de donnees statistique", 5);
        assert_eq!(hits[0].name, "melodi_search_melodi_datasets");
    }

    #[test]
    fn finds_tool_by_server_domain() {
        // The server's domain is indexed: "géographie" brings up the geodata tools.
        let hits = index().search("géographie", 5);
        assert!(
            hits.iter().all(|h| h.name.starts_with("geodata_")),
            "{hits:?}"
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn finds_tool_by_parameter_description() {
        let hits = index().search("adresse", 5);
        assert_eq!(hits[0].name, "geodata_geocode");
    }

    #[test]
    fn respects_limit_and_handles_no_match() {
        assert_eq!(index().search("géographie", 1).len(), 1);
        assert!(index().search("zzzz", 5).is_empty());
    }
}
