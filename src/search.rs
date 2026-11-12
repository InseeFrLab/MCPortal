//! # Recherche d'outils (BM25)
//!
//! `search_tools` fait une recherche plein texte sur les outils de tous les amonts.
//! On s'appuie sur le crate [`bm25`] (pur Rust, léger) plutôt que sur un moteur complet
//! (tantivy, DuckDB) : il y a au plus quelques centaines d'outils, l'index se reconstruit en
//! quelques millisecondes et tient en mémoire.
//!
//! ## Ce qui fait la qualité de la recherche
//!
//! La qualité dépend surtout du **document** indexé pour chaque outil ([`tool_document`]) :
//!
//! - le nom de l'outil **découpé** (`search_melodi_datasets` → « search melodi datasets »),
//!   répété deux fois pour lui donner plus de poids (le « boost du pauvre » en BM25) ;
//! - le nom, le domaine, les tags et la description **du serveur** : une recherche sur un
//!   domaine (« diffusion ») trouve ainsi les outils du serveur correspondant ;
//! - la description de l'outil ;
//! - les noms et descriptions de ses **paramètres** (`geo_level`, `dataset_id`…).
//!
//! Le tokenizer est réglé pour le français : suppression des accents, mots vides
//! (« le », « des »…) et racinisation (« publications » et « publication » se rejoignent).
//!
//! ## Pourquoi un trait ?
//!
//! [`ToolIndex`] isole la recherche du reste de la passerelle. Ce module ne dépend que de
//! `rmcp::model::Tool` : il pourra être extrait tel quel (crate séparé ou PR dans rmcp).

use bm25::{Document, Language, LanguageMode, SearchEngine, SearchEngineBuilder};
use rmcp::model::Tool;

/// Un résultat de recherche : le nom exposé de l'outil et son score BM25.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredTool {
    pub name: String,
    pub score: f32,
}

/// Interface minimale d'un index de recherche d'outils.
pub trait ToolIndex: Send + Sync {
    /// Renvoie au plus `limit` outils, du plus pertinent au moins pertinent.
    fn search(&self, query: &str, limit: usize) -> Vec<ScoredTool>;
}

/// Ce que l'on sait d'un outil au moment de l'indexer.
pub struct IndexEntry<'a> {
    /// Nom exposé par la passerelle (`{serveur}_{outil}`), utilisé comme identifiant.
    pub exposed_name: &'a str,
    pub tool: &'a Tool,
    /// Texte décrivant le serveur (nom, domaine, tags, description).
    pub server_text: &'a str,
}

/// Index BM25 construit une fois pour toutes (immuable) à partir d'une liste d'outils.
pub struct Bm25ToolIndex {
    /// Les identifiants de documents sont des indices dans `names` (un entier est plus
    /// efficace qu'une chaîne pour le crate bm25).
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
        // La requête subit le même pré-traitement que les documents (découpage des `_`).
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

/// Construit le texte indexé pour un outil. Voir la documentation du module.
pub fn tool_document(entry: &IndexEntry<'_>) -> String {
    let tool = entry.tool;
    let mut parts: Vec<String> = Vec::new();

    // Nom exposé, deux fois (boost) : il contient le préfixe serveur et le nom amont.
    parts.push(entry.exposed_name.to_owned());
    parts.push(entry.exposed_name.to_owned());
    if let Some(title) = &tool.title {
        parts.push(title.clone());
    }
    parts.push(entry.server_text.to_owned());
    if let Some(description) = &tool.description {
        parts.push(description.to_string());
    }

    // Paramètres : `inputSchema.properties.{nom}.description`.
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

/// Remplace `_` et `-` par des espaces : le découpage Unicode des mots considère
/// `search_melodi_datasets` comme **un seul** mot, ce qui rendrait « melodi » introuvable.
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
        // « melodi » n'apparaît que dans des identifiants avec `_` : le découpage est essentiel.
        let hits = index().search("melodi", 5);
        assert_eq!(hits[0].name, "melodi_search_melodi_datasets");
    }

    #[test]
    fn ignores_accents_and_plural() {
        // « donnees » sans accent et au pluriel doit trouver « données ».
        let hits = index().search("jeu de donnees statistique", 5);
        assert_eq!(hits[0].name, "melodi_search_melodi_datasets");
    }

    #[test]
    fn finds_tool_by_server_domain() {
        // Le domaine du serveur est indexé : « géographie » remonte les outils geodata.
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
