//! Schema inference from actual note usage.
//!
//! Ports `basic_memory.picoschema.inference`. Frequency analysis counts a field once
//! per note (presence, not occurrences); array-ness is inferred when more than half
//! of the notes containing a field contain it *more than once*.
//!
//! Two ordering rules are observable and must be preserved: `Counter.most_common`
//! sorts by count descending and keeps insertion order for ties, and the suggested
//! schema is built by walking that order.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Map, Value};

/// One observation presented to the inference engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationData {
    /// Observation category.
    pub category: String,
    /// Observation content.
    pub content: String,
}

/// One relation presented to the inference engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationData {
    /// Relation label.
    pub relation_type: String,
    /// Raw target name.
    pub target_name: String,
    /// Resolved target note type, when known.
    pub target_note_type: Option<String>,
}

/// One note presented to the inference engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteData {
    /// Reporting identifier.
    pub identifier: String,
    /// The note's observations.
    pub observations: Vec<ObservationData>,
    /// The note's relations.
    pub relations: Vec<RelationData>,
}

/// Frequency analysis for one field across notes of a type.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldFrequency {
    /// Field name.
    pub name: String,
    /// `observation` or `relation`.
    pub source: String,
    /// Notes containing this field.
    pub count: usize,
    /// Notes analyzed.
    pub total: usize,
    /// `count / total`.
    pub percentage: f64,
    /// Up to `max_sample_values` distinct values.
    pub sample_values: Vec<String>,
    /// Appears more than once in the majority of the notes that have it.
    pub is_array: bool,
    /// Most common target note type (relations only).
    pub target_type: Option<String>,
}

/// Complete inference result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InferenceResult {
    /// Analyzed note type.
    pub note_type: String,
    /// Notes analyzed.
    pub notes_analyzed: usize,
    /// Per-field frequencies, most common first.
    pub field_frequencies: Vec<FieldFrequency>,
    /// Suggested Picoschema mapping, in frequency order.
    pub suggested_schema: Map<String, Value>,
    /// Fields at or above the required threshold.
    pub suggested_required: Vec<String>,
    /// Fields at or above the optional threshold but below required.
    pub suggested_optional: Vec<String>,
    /// Fields below the optional threshold.
    pub excluded: Vec<String>,
}

/// A growing counter that remembers first-insertion order for stable ties.
#[derive(Default)]
struct OrderedCounter {
    order: Vec<String>,
    counts: HashMap<String, usize>,
}

impl OrderedCounter {
    fn bump(&mut self, key: &str) {
        if !self.counts.contains_key(key) {
            self.order.push(key.to_owned());
        }
        *self.counts.entry(key.to_owned()).or_insert(0) += 1;
    }

    fn get(&self, key: &str) -> usize {
        self.counts.get(key).copied().unwrap_or(0)
    }

    /// Entries in first-insertion order, matching a Python `dict`'s iteration order.
    fn entries(&self) -> Vec<(String, usize)> {
        self.order
            .iter()
            .map(|key| (key.clone(), self.counts[key]))
            .collect()
    }

    /// `Counter.most_common`: count descending, insertion order for ties.
    fn most_common(&self) -> Vec<(String, usize)> {
        let mut items = self.entries();
        // Stable descending sort: ties keep first-appearance order, matching
        // `Counter.most_common`.
        items.sort_by_key(|item| std::cmp::Reverse(item.1));
        items
    }
}

/// Analyze notes and suggest a Picoschema definition.
pub fn infer_schema(
    note_type: &str,
    notes: &[NoteData],
    required_threshold: f64,
    optional_threshold: f64,
    max_sample_values: usize,
) -> InferenceResult {
    let total = notes.len();
    if total == 0 {
        return InferenceResult {
            note_type: note_type.to_owned(),
            notes_analyzed: 0,
            field_frequencies: Vec::new(),
            suggested_schema: Map::new(),
            suggested_required: Vec::new(),
            suggested_optional: Vec::new(),
            excluded: Vec::new(),
        };
    }

    let mut frequencies = analyze_observations(notes, total, max_sample_values);
    frequencies.extend(analyze_relations(notes, total, max_sample_values));

    let mut suggested_required = Vec::new();
    let mut suggested_optional = Vec::new();
    let mut excluded = Vec::new();
    for frequency in &frequencies {
        if frequency.percentage >= required_threshold {
            suggested_required.push(frequency.name.clone());
        } else if frequency.percentage >= optional_threshold {
            suggested_optional.push(frequency.name.clone());
        } else {
            excluded.push(frequency.name.clone());
        }
    }

    let suggested_schema =
        build_picoschema_dict(&frequencies, required_threshold, optional_threshold);
    InferenceResult {
        note_type: note_type.to_owned(),
        notes_analyzed: total,
        field_frequencies: frequencies,
        suggested_schema,
        suggested_required,
        suggested_optional,
        excluded,
    }
}

/// Count observation category frequencies across notes.
pub fn analyze_observations(
    notes: &[NoteData],
    total: usize,
    max_sample_values: usize,
) -> Vec<FieldFrequency> {
    let mut note_count = OrderedCounter::default();
    let mut multi_count = OrderedCounter::default();
    let mut samples: HashMap<String, Vec<String>> = HashMap::new();

    for note in notes {
        let mut per_note: OrderedCounter = OrderedCounter::default();
        let mut values: HashMap<String, Vec<String>> = HashMap::new();
        for observation in &note.observations {
            per_note.bump(&observation.category);
            values
                .entry(observation.category.clone())
                .or_default()
                .push(observation.content.clone());
        }
        // Per-note categories are walked in first-appearance order (`dict.items()`
        // in the reference), which is what decides tie order in the global counter.
        for (category, count) in per_note.entries() {
            note_count.bump(&category);
            if count > 1 {
                multi_count.bump(&category);
            }
            let bucket = samples.entry(category.clone()).or_default();
            for value in &values[&category] {
                if !bucket.contains(value) && bucket.len() < max_sample_values {
                    bucket.push(value.clone());
                }
            }
        }
    }

    note_count
        .most_common()
        .into_iter()
        .map(|(category, count)| {
            let multi = multi_count.get(&category);
            FieldFrequency {
                is_array: multi as f64 > count as f64 / 2.0,
                sample_values: samples.get(&category).cloned().unwrap_or_default(),
                name: category,
                source: "observation".to_owned(),
                count,
                total,
                percentage: count as f64 / total as f64,
                target_type: None,
            }
        })
        .collect()
}

/// Count relation type frequencies across notes.
pub fn analyze_relations(
    notes: &[NoteData],
    total: usize,
    max_sample_values: usize,
) -> Vec<FieldFrequency> {
    let mut note_count = OrderedCounter::default();
    let mut multi_count = OrderedCounter::default();
    let mut samples: HashMap<String, Vec<String>> = HashMap::new();
    let mut targets: HashMap<String, OrderedCounter> = HashMap::new();

    for note in notes {
        let mut per_note: OrderedCounter = OrderedCounter::default();
        let mut values: HashMap<String, Vec<String>> = HashMap::new();
        let mut per_note_relations: HashMap<String, Vec<&RelationData>> = HashMap::new();
        for relation in &note.relations {
            per_note.bump(&relation.relation_type);
            values
                .entry(relation.relation_type.clone())
                .or_default()
                .push(relation.target_name.clone());
            per_note_relations
                .entry(relation.relation_type.clone())
                .or_default()
                .push(relation);
        }
        // First-appearance order, as above: only the global `most_common` reorders.
        for (relation_type, count) in per_note.entries() {
            note_count.bump(&relation_type);
            if count > 1 {
                multi_count.bump(&relation_type);
            }
            let bucket = samples.entry(relation_type.clone()).or_default();
            for value in &values[&relation_type] {
                if !bucket.contains(value) && bucket.len() < max_sample_values {
                    bucket.push(value.clone());
                }
            }
            let counter = targets.entry(relation_type.clone()).or_default();
            for relation in &per_note_relations[&relation_type] {
                if let Some(note_type) = &relation.target_note_type {
                    counter.bump(note_type);
                }
            }
        }
    }

    note_count
        .most_common()
        .into_iter()
        .map(|(relation_type, count)| {
            let multi = multi_count.get(&relation_type);
            let target_type = targets
                .get(&relation_type)
                .and_then(|counter| counter.most_common().first().map(|(name, _)| name.clone()));
            FieldFrequency {
                is_array: multi as f64 > count as f64 / 2.0,
                sample_values: samples.get(&relation_type).cloned().unwrap_or_default(),
                name: relation_type,
                source: "relation".to_owned(),
                count,
                total,
                percentage: count as f64 / total as f64,
                target_type,
            }
        })
        .collect()
}

/// Build the suggested Picoschema mapping, keeping only fields at or above the
/// optional threshold.
fn build_picoschema_dict(
    frequencies: &[FieldFrequency],
    required_threshold: f64,
    optional_threshold: f64,
) -> Map<String, Value> {
    let mut schema = Map::new();
    for frequency in frequencies {
        if frequency.percentage < optional_threshold {
            continue;
        }
        let is_required = frequency.percentage >= required_threshold;
        let mut key = frequency.name.clone();
        if !is_required {
            key.push('?');
        }
        if frequency.is_array {
            key.push_str("(array)");
        }
        let value = if frequency.source == "relation" {
            match frequency.target_type.as_deref() {
                Some(target) if target != "string" => {
                    let mut characters = target.chars();
                    match characters.next() {
                        Some(first) => {
                            first.to_uppercase().collect::<String>() + characters.as_str()
                        }
                        None => "string".to_owned(),
                    }
                }
                _ => "string".to_owned(),
            }
        } else {
            "string".to_owned()
        };
        schema.insert(key, Value::String(value));
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::{NoteData, ObservationData, RelationData, analyze_observations, analyze_relations};

    fn note(identifier: &str, categories: &[(&str, &str)]) -> NoteData {
        NoteData {
            identifier: identifier.to_owned(),
            observations: categories
                .iter()
                .map(|(category, content)| ObservationData {
                    category: (*category).to_owned(),
                    content: (*content).to_owned(),
                })
                .collect(),
            relations: Vec::new(),
        }
    }

    /// A repeated category must not jump ahead of an earlier singleton *within* a
    /// note: the reference walks each note's categories in first-appearance order and
    /// only reorders globally, so ties keep that order.
    #[test]
    fn ties_follow_first_appearance_within_a_note() {
        let notes = vec![
            note(
                "ada",
                &[
                    ("name", "Ada Lovelace"),
                    ("role", "Mathematician"),
                    ("tags", "pioneer"),
                    ("tags", "history"),
                    ("status", "active"),
                ],
            ),
            note(
                "grace",
                &[
                    ("name", "Grace Hopper"),
                    ("status", "retired"),
                    ("hobby", "sailing"),
                ],
            ),
        ];

        let frequencies = analyze_observations(&notes, 2, 5);
        let names: Vec<&str> = frequencies
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["name", "status", "role", "tags", "hobby"]);
        // `tags` appears twice in the one note that has it, so it reads as an array.
        let tags = frequencies
            .iter()
            .find(|field| field.name == "tags")
            .expect("tags");
        assert!(tags.is_array);
        assert_eq!(tags.sample_values, ["pioneer", "history"]);
    }

    /// Relation types follow the same rule, and carry their target note type.
    #[test]
    fn relation_ties_follow_first_appearance() {
        let mut note = note("ada", &[("name", "Ada Lovelace")]);
        note.relations = vec![
            RelationData {
                relation_type: "works_at".to_owned(),
                target_name: "organizations/analytical-engine".to_owned(),
                target_note_type: Some("organization".to_owned()),
            },
            RelationData {
                relation_type: "notes_about".to_owned(),
                target_name: "notes/simple".to_owned(),
                target_note_type: Some("note".to_owned()),
            },
            RelationData {
                relation_type: "notes_about".to_owned(),
                target_name: "notes/relations".to_owned(),
                target_note_type: Some("note".to_owned()),
            },
        ];

        let frequencies = analyze_relations(&[note], 1, 5);
        let names: Vec<&str> = frequencies
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["works_at", "notes_about"]);
        assert_eq!(frequencies[0].target_type.as_deref(), Some("organization"));
        assert!(!frequencies[0].is_array);
        assert!(frequencies[1].is_array);
    }
}
