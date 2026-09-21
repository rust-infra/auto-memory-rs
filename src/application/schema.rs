//! Schema tool surface: validation, inference, and drift reports.
//!
//! Ports the report-assembly half of `basic_memory.api.v2.routers.schema_router`:
//! which notes a request covers, how schemas are looked up, and how the core results
//! are shaped into the `ValidationReport` / `InferenceReport` / `DriftReport`
//! payloads the MCP tools and CLI return.
//!
//! Two lookup rules are observable and must be preserved:
//!
//! * note types are compared through [`normalize_note_type`], because indexed
//!   frontmatter keeps whatever spelling the author used;
//! * a schema note's *file* is the source of truth for its definition, with the
//!   indexed metadata only as a fallback when the file is missing or incomplete.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde_json::{Map, Value};

use crate::domain::note_type::normalize_note_type;
use crate::domain::permalink::generate_permalink;
use crate::error::{Error, Result};
use crate::markdown::serialize::split_frontmatter;
use crate::schema::diff::diff_schema;
use crate::schema::inference::{NoteData, ObservationData, RelationData, infer_schema};
use crate::schema::parser::{SchemaDefinition, SchemaParseError, parse_schema_note};
use crate::schema::report::{
    DriftReport, InferenceReport, NoteValidationResponse, TypeValidationSummary, ValidationReport,
};
use crate::schema::resolver::schema_from_inline;
use crate::schema::validator::validate_note;
use crate::search::text::TextSearchOptions;
use crate::storage::{EntityRow, RelationRow, Store};

/// Required-field threshold for inference (`infer_schema` default).
pub const REQUIRED_THRESHOLD: f64 = 0.95;
/// Optional-field threshold used when the caller does not override it.
pub const OPTIONAL_THRESHOLD: f64 = 0.25;
/// Sample values kept per field by inference.
pub const MAX_SAMPLE_VALUES: usize = 5;
/// Field frequency above which an undeclared field counts as drift.
pub const NEW_FIELD_THRESHOLD: f64 = 0.25;
/// Field frequency below which a declared field counts as drift.
pub const DROPPED_FIELD_THRESHOLD: f64 = 0.10;

/// Schema reporting bound to one indexed project and its vault.
pub struct SchemaService<'a> {
    store: &'a Store,
    project_id: i64,
    vault: &'a Path,
}

impl<'a> SchemaService<'a> {
    /// Create a service for one project.
    pub fn new(store: &'a Store, project_id: i64, vault: &'a Path) -> Self {
        Self {
            store,
            project_id,
            vault,
        }
    }

    /// Validate one note, every note of a type, or every schema-covered type.
    pub async fn validate(
        &self,
        note_type: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<ValidationReport> {
        if let Some(identifier) = identifier {
            return self.validate_identifier(note_type, identifier).await;
        }

        if let Some(note_type) = note_type {
            let canonical = normalize_note_type(note_type);
            let entities = self.notes_of_type(&canonical).await?;
            let results = self.validate_entities(&entities).await?;
            return Ok(ValidationReport::new(
                Some(canonical),
                entities.len(),
                results,
            ));
        }

        // Neither argument: validate every type that has a schema defined, with a
        // per-type breakdown. An empty breakdown means "no schemas defined".
        let mut results = Vec::new();
        let mut summaries = Vec::new();
        let mut total_entities = 0;
        for (display_label, _stored) in self.schema_covered_note_types().await? {
            let entities = self.notes_of_type(&display_label).await?;
            let type_results = self.validate_entities(&entities).await?;
            summaries.push(TypeValidationSummary::new(
                display_label,
                entities.len(),
                &type_results,
            ));
            total_entities += entities.len();
            results.extend(type_results);
        }

        let mut report = ValidationReport::new(None, total_entities, results);
        report.type_summaries = summaries;
        Ok(report)
    }

    /// Infer a schema definition from every note of one type.
    pub async fn infer(&self, note_type: &str, threshold: f64) -> Result<InferenceReport> {
        let canonical = normalize_note_type(note_type);
        let entities = self.notes_of_type(&canonical).await?;
        let notes = self.notes_data(&entities).await?;
        let result = infer_schema(
            &canonical,
            &notes,
            REQUIRED_THRESHOLD,
            threshold,
            MAX_SAMPLE_VALUES,
        );
        Ok(InferenceReport::from(&result))
    }

    /// Compare a type's schema against how its notes are actually structured.
    pub async fn diff(&self, note_type: &str) -> Result<DriftReport> {
        let canonical = normalize_note_type(note_type);
        let mut frontmatter = Map::new();
        frontmatter.insert("type".to_owned(), Value::String(canonical.clone()));
        let frontmatter = Value::Object(frontmatter);

        let schema = match self.resolve_schema_for(&frontmatter).await? {
            Some(schema) => schema,
            None => return Ok(DriftReport::missing_schema(canonical)),
        };

        let entities = self.notes_of_type(&canonical).await?;
        let notes = self.notes_data(&entities).await?;
        let drift = diff_schema(
            &schema,
            &notes,
            NEW_FIELD_THRESHOLD,
            DROPPED_FIELD_THRESHOLD,
        );
        // The success path reports the *caller's* spelling, matching the router.
        Ok(DriftReport::from_drift(note_type, &drift))
    }

    /// Map every schema-covered type to the stored spellings it covers.
    ///
    /// Mirrors `_schema_covered_note_types`. Coverage comes from standalone schema
    /// notes *and* from notes carrying an inline schema or an explicit reference; the
    /// returned order is by normalized type, and the first spelling seen for a type
    /// becomes its display label.
    pub async fn schema_covered_note_types(&self) -> Result<Vec<(String, Vec<String>)>> {
        let entities = self.store.entities(self.project_id).await?;
        let mut targets: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();

        for entity in entities
            .iter()
            .filter(|entity| entity.note_type == "schema")
        {
            if let Some(target) = entity.metadata.get("entity").and_then(Value::as_str)
                && !target.is_empty()
            {
                targets
                    .entry(normalize_note_type(target))
                    .or_insert_with(|| (target.to_owned(), BTreeSet::new()));
            }
        }

        for entity in entities
            .iter()
            .filter(|entity| entity.note_type != "schema")
        {
            if entity.note_type.is_empty() {
                continue;
            }
            let normalized = normalize_note_type(&entity.note_type);
            if let Some((_, stored_types)) = targets.get_mut(&normalized) {
                stored_types.insert(entity.note_type.clone());
            }

            let has_direct_schema = match entity.metadata.get("schema") {
                Some(Value::Object(_)) => true,
                Some(Value::String(reference)) => !reference.is_empty(),
                _ => false,
            };
            if has_direct_schema {
                targets
                    .entry(normalized)
                    .or_insert_with(|| (entity.note_type.clone(), BTreeSet::new()))
                    .1
                    .insert(entity.note_type.clone());
            }
        }

        Ok(targets
            .into_iter()
            .map(|(_, (display_label, stored_types))| {
                (display_label, stored_types.into_iter().collect())
            })
            .collect())
    }

    // --- Validation paths ---

    async fn validate_identifier(
        &self,
        note_type: Option<&str>,
        identifier: &str,
    ) -> Result<ValidationReport> {
        let Some(entity) = self.resolve_identifier(identifier).await? else {
            // An unknown identifier is an empty report, not an error.
            return Ok(ValidationReport::new(
                note_type.map(str::to_owned),
                0,
                Vec::new(),
            ));
        };

        let frontmatter = entity_frontmatter(&entity);
        let mut results = Vec::new();
        if let Some(schema) = self.resolve_schema_for(&frontmatter).await? {
            results.push(self.validate_entity(&entity, &schema, &frontmatter).await?);
        }

        let resolved_type = note_type
            .map(str::to_owned)
            .or_else(|| (!entity.note_type.is_empty()).then(|| entity.note_type.clone()));
        // The identifier path always reports one entity, whether or not a schema ran.
        Ok(ValidationReport::new(resolved_type, 1, results))
    }

    /// Resolve a note identifier the way `LinkResolver.resolve_link` does.
    ///
    /// The reference walks permalink candidates, then an exact title, then the file
    /// path (with and without `.md`), and finally falls back to search — taking only
    /// the best hit. Kept here rather than in the graph module because the schema
    /// tools are the local surface that needs the forgiving end of that chain: a
    /// search hit that names a *different* note is still the reference's answer.
    async fn resolve_identifier(&self, identifier: &str) -> Result<Option<EntityRow>> {
        if let Some(entity) =
            crate::graph::resolve_entity_path(self.store, self.project_id, identifier).await?
        {
            return Ok(Some(entity));
        }

        if let Some(entity) = self
            .store
            .entities_by_title(self.project_id, identifier)
            .await?
            .into_iter()
            .next()
        {
            return Ok(Some(entity));
        }

        // Fuzzy fallback: the best search hit decides, and a hit without a permalink
        // ends the chain rather than moving on to the next result.
        if identifier.contains('*') {
            return Ok(None);
        }
        let found = self
            .store
            .search_text(
                self.project_id,
                &TextSearchOptions {
                    query: Some(identifier.to_owned()),
                    ..TextSearchOptions::default()
                },
            )
            .await?;
        let Some(permalink) = found.results.first().and_then(|row| row.permalink.clone()) else {
            return Ok(None);
        };
        self.store
            .entity_by_permalink(self.project_id, &permalink)
            .await
    }

    async fn validate_entities(
        &self,
        entities: &[EntityRow],
    ) -> Result<Vec<NoteValidationResponse>> {
        let relations = self.store.relations(self.project_id).await?;
        let target_types = self.entity_target_types().await?;
        let mut results = Vec::new();
        for entity in entities {
            let frontmatter = entity_frontmatter(entity);
            // Entities whose frontmatter resolves to no schema are skipped, which is
            // why a report's `total_notes` can be lower than its `total_entities`.
            if let Some(schema) = self.resolve_schema_for(&frontmatter).await? {
                let observations = self.entity_observations(entity).await?;
                let entity_relations = relations_for(&relations, &target_types, entity.id);
                let result = validate_note(
                    &reporting_identifier(entity, None),
                    &schema,
                    &observations,
                    &entity_relations,
                    Some(&frontmatter),
                );
                results.push(NoteValidationResponse::from_result(&result));
            }
        }
        Ok(results)
    }

    async fn validate_entity(
        &self,
        entity: &EntityRow,
        schema: &SchemaDefinition,
        frontmatter: &Value,
    ) -> Result<NoteValidationResponse> {
        let observations = self.entity_observations(entity).await?;
        let relations = self.store.relations(self.project_id).await?;
        let target_types = self.entity_target_types().await?;
        let result = validate_note(
            &reporting_identifier(entity, None),
            schema,
            &observations,
            &relations_for(&relations, &target_types, entity.id),
            Some(frontmatter),
        );
        Ok(NoteValidationResponse::from_result(&result))
    }

    // --- Schema resolution ---

    /// Resolve the schema a note's frontmatter points at.
    ///
    /// `allow_reference_match` is decided per query, exactly as the router's closure
    /// does: only a lookup whose query equals the note's own `schema` reference may
    /// fall back to matching schema notes by title or permalink.
    async fn resolve_schema_for(&self, frontmatter: &Value) -> Result<Option<SchemaDefinition>> {
        let frontmatter_value = frontmatter;
        let empty = Map::new();
        let frontmatter = frontmatter.as_object().unwrap_or(&empty);
        let schema_ref = frontmatter.get("schema");
        let allow_reference_match = |query: &str| {
            schema_ref.is_some_and(Value::is_string)
                && schema_ref.and_then(Value::as_str) == Some(query)
        };

        if let Some(Value::Object(schema_dict)) = frontmatter.get("schema") {
            return schema_from_inline(schema_dict, frontmatter_value)
                .map(Some)
                .map_err(schema_parse_error);
        }

        if let Some(Value::String(reference)) = frontmatter.get("schema")
            && let Some(first) = self
                .schema_frontmatters_for(reference, allow_reference_match(reference))
                .await?
                .first()
        {
            return parse_schema_note(first)
                .map(Some)
                .map_err(schema_parse_error);
        }

        if let Some(note_type) = frontmatter
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            && let Some(first) = self
                .schema_frontmatters_for(note_type, allow_reference_match(note_type))
                .await?
                .first()
        {
            return parse_schema_note(first)
                .map(Some)
                .map_err(schema_parse_error);
        }

        Ok(None)
    }

    /// Look up schema notes for a resolver query, returning their frontmatter.
    ///
    /// Mirrors `_find_schema_entities` plus `_schema_frontmatter_from_file`.
    async fn schema_frontmatters_for(
        &self,
        query: &str,
        allow_reference_match: bool,
    ) -> Result<Vec<Value>> {
        Ok(self
            .schema_entities_for(query, allow_reference_match)
            .await?
            .iter()
            .map(|entity| self.schema_frontmatter(entity))
            .collect())
    }

    async fn schema_entities_for(
        &self,
        target: &str,
        allow_reference_match: bool,
    ) -> Result<Vec<EntityRow>> {
        let schemas: Vec<EntityRow> = self
            .store
            .entities(self.project_id)
            .await?
            .into_iter()
            .filter(|entity| entity.note_type == "schema")
            .collect();

        let normalized_target = normalize_note_type(target);
        let entity_matches: Vec<EntityRow> = schemas
            .iter()
            .filter(|entity| {
                entity
                    .metadata
                    .get("entity")
                    .and_then(Value::as_str)
                    .is_some_and(|value| normalize_note_type(value) == normalized_target)
            })
            .cloned()
            .collect();
        if !entity_matches.is_empty() || !allow_reference_match {
            return Ok(entity_matches);
        }

        let target_reference = generate_permalink(target);
        Ok(schemas
            .into_iter()
            .filter(|entity| {
                let mut candidates = vec![entity.title.clone()];
                if let Some(permalink) = entity.permalink.as_deref() {
                    candidates.push(permalink.to_owned());
                    if let Some(name) = permalink.rsplit('/').next() {
                        candidates.push(name.to_owned());
                    }
                }
                candidates
                    .iter()
                    .any(|reference| generate_permalink(reference) == target_reference)
            })
            .collect())
    }

    /// Frontmatter for one schema entity: the file wins, the index is the fallback.
    fn schema_frontmatter(&self, entity: &EntityRow) -> Value {
        let from_file = std::fs::read_to_string(self.vault.join(&entity.file_path))
            .ok()
            .and_then(|content| split_frontmatter(&content).ok().flatten())
            .map(|(mapping, _)| Value::Object(raw_yaml_to_json_object(&mapping)));

        match from_file {
            // Trigger: the file is mid-edit and missing required schema fields.
            // Why: `parse_schema_note` raises for a missing entity/schema, which would
            // turn validation into a hard failure.
            // Outcome: fall back to last-known-good database metadata.
            Some(metadata) if is_complete_schema_frontmatter(&metadata) => metadata,
            _ => entity_frontmatter(entity),
        }
    }

    // --- Note-type queries ---

    /// Every note whose stored type normalizes to `note_type`.
    async fn notes_of_type(&self, note_type: &str) -> Result<Vec<EntityRow>> {
        let canonical = normalize_note_type(note_type);
        let entities = self.store.entities(self.project_id).await?;
        let stored_types: BTreeSet<String> = entities
            .iter()
            .map(|entity| entity.note_type.clone())
            .filter(|stored| !stored.is_empty() && normalize_note_type(stored) == canonical)
            .collect();
        if stored_types.is_empty() {
            return Ok(Vec::new());
        }
        Ok(entities
            .into_iter()
            .filter(|entity| stored_types.contains(&entity.note_type))
            .collect())
    }

    // --- Entity projection ---

    async fn entity_target_types(&self) -> Result<HashMap<i64, String>> {
        Ok(self
            .store
            .entities(self.project_id)
            .await?
            .into_iter()
            .map(|entity| (entity.id, entity.note_type))
            .collect())
    }

    async fn entity_observations(&self, entity: &EntityRow) -> Result<Vec<ObservationData>> {
        Ok(self
            .store
            .observations_for_entity(entity.id)
            .await?
            .into_iter()
            .map(|observation| ObservationData {
                category: observation.category,
                content: observation.content,
            })
            .collect())
    }

    /// Project entities as inference/diff input rows.
    async fn notes_data(&self, entities: &[EntityRow]) -> Result<Vec<NoteData>> {
        let relations = self.store.relations(self.project_id).await?;
        let target_types = self.entity_target_types().await?;
        let mut notes = Vec::with_capacity(entities.len());
        for entity in entities {
            notes.push(NoteData {
                identifier: reporting_identifier(entity, None),
                observations: self.entity_observations(entity).await?,
                relations: relations_for(&relations, &target_types, entity.id),
            });
        }
        Ok(notes)
    }
}

/// Outgoing relations of one entity, carrying their target's note type.
fn relations_for(
    relations: &[RelationRow],
    target_types: &HashMap<i64, String>,
    entity_id: i64,
) -> Vec<RelationData> {
    relations
        .iter()
        .filter(|relation| relation.from_id == entity_id)
        .map(|relation| RelationData {
            relation_type: relation.relation_type.clone(),
            target_name: relation.to_name.clone(),
            target_note_type: relation.to_id.and_then(|id| target_types.get(&id).cloned()),
        })
        .collect()
}

/// The identifier a report names a note by: title, then permalink, then the path.
///
/// The reference's single-note path falls back to the caller's identifier rather than
/// the file path, but a note always has a title by the time it is indexed, so the two
/// chains agree in practice.
fn reporting_identifier(entity: &EntityRow, fallback: Option<&str>) -> String {
    if !entity.title.is_empty() {
        return entity.title.clone();
    }
    if let Some(permalink) = entity.permalink.clone() {
        return permalink;
    }
    fallback.unwrap_or(&entity.file_path).to_owned()
}

/// Frontmatter for a note under validation: indexed metadata plus its type.
pub fn entity_frontmatter(entity: &EntityRow) -> Value {
    let mut metadata = entity.metadata.clone();
    if !entity.note_type.is_empty() {
        metadata
            .entry("type".to_owned())
            .or_insert_with(|| Value::String(entity.note_type.clone()));
    }
    Value::Object(metadata)
}

/// `metadata.get("entity")` truthy *and* `metadata["schema"]` a mapping.
fn is_complete_schema_frontmatter(metadata: &Value) -> bool {
    let Some(mapping) = metadata.as_object() else {
        return false;
    };
    let has_entity = mapping.get("entity").is_some_and(|value| !is_falsy(value));
    let has_schema = mapping.get("schema").is_some_and(Value::is_object);
    has_entity && has_schema
}

/// Python truthiness for the YAML scalars that reach schema frontmatter.
fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Number(number) => number.as_f64().is_none_or(|value| value == 0.0),
    }
}

/// Convert raw YAML frontmatter to JSON, keeping scalar kinds.
///
/// The reference reads schema files with `frontmatter.loads`, whose metadata keeps
/// native ints and bools (unlike the string-normalized metadata stored in the index),
/// so a schema author's `version: 1` stays a number here as well.
pub fn raw_yaml_to_json_object(mapping: &serde_yaml_ng::Mapping) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, value) in mapping {
        let key = match key {
            serde_yaml_ng::Value::String(text) => text.clone(),
            other => serde_yaml_ng::to_string(other)
                .unwrap_or_default()
                .trim()
                .to_owned(),
        };
        out.insert(key, raw_yaml_to_json(value));
    }
    out
}

fn raw_yaml_to_json(value: &serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Yaml;
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(flag) => Value::Bool(*flag),
        Yaml::Number(number) => number
            .as_i64()
            .map(Value::from)
            .or_else(|| number.as_u64().map(Value::from))
            .or_else(|| {
                number
                    .as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
            })
            .unwrap_or_else(|| Value::String(number.to_string())),
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Sequence(items) => Value::Array(items.iter().map(raw_yaml_to_json).collect()),
        Yaml::Mapping(mapping) => Value::Object(raw_yaml_to_json_object(mapping)),
        Yaml::Tagged(tagged) => raw_yaml_to_json(&tagged.value),
    }
}

/// Surface a schema authoring error exactly as the reference does.
///
/// The reference turns the parser's `ValueError` into an HTTP 400 whose detail is the
/// message verbatim; the MCP tools render that text inside their failure template.
fn schema_parse_error(error: SchemaParseError) -> Error {
    Error::InvalidArgument {
        message: error.message,
    }
}
