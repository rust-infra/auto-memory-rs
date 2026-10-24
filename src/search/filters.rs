//! Metadata filter compilation: the `metadata_filters` argument of `search_notes`.
//!
//! Upstream 0.24.0 (#1676) accepts the shapes agents actually write, instead of the one
//! shape the original port supported (`{"key": "value"}` only):
//!
//! ```text
//! {"status": "active"}                       exact match
//! {"priority": {"$gte": 2}}                  operator object
//! {"priority": {"$gte": 2, "$lte": 8}}       several operators on one field
//! {"priority": {"gte": 2}}                   bare operator name, as an alias
//! {"tags": {"$contains": "rust"}}            array contains
//! {"reviewed": {"$exists": true}}            presence
//! {"status": {"$in": ["active", "done"]}}    membership
//! ```
//!
//! Two rules keep this predictable:
//!
//! - an unknown operator is an **error** that lists the valid ones, because silently
//!   ignoring a filter answers a different question than the caller asked;
//! - the field name is restricted to an identifier-shaped key, since it is interpolated
//!   into a JSON path — anything else is refused rather than escaped.

use std::collections::BTreeMap;

use rusqlite::types::Value as SqlValue;
use serde_json::Value;

use crate::error::{Error, Result};

/// The operators `metadata_filters` understands, with the SQL each compiles to.
///
/// Ordered for the error message, which lists them in a stable order.
pub const OPERATORS: [&str; 8] = [
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$contains",
];

/// One comparison on one frontmatter field.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterOp {
    /// `= value`
    Eq(Value),
    /// `<> value` (a missing key does not match, as in the reference)
    Ne(Value),
    /// `> value`
    Gt(Value),
    /// `>= value`
    Gte(Value),
    /// `< value`
    Lt(Value),
    /// `<= value`
    Lte(Value),
    /// `IN (...)`
    In(Vec<Value>),
    /// The array stored at the key contains the value.
    Contains(Value),
    /// The key is present (`true`) or absent (`false`).
    Exists(bool),
}

impl FilterOp {
    /// The operator's wire name, for error messages and diagnostics.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Eq(_) => "$eq",
            Self::Ne(_) => "$ne",
            Self::Gt(_) => "$gt",
            Self::Gte(_) => "$gte",
            Self::Lt(_) => "$lt",
            Self::Lte(_) => "$lte",
            Self::In(_) => "$in",
            Self::Contains(_) => "$contains",
            Self::Exists(_) => "$exists",
        }
    }
}

/// One frontmatter field and every comparison applied to it.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldFilter {
    /// Frontmatter key (identifier-shaped).
    pub key: String,
    /// Comparisons, ANDed together.
    pub ops: Vec<FilterOp>,
}

/// Parse the raw `metadata_filters` object into typed filters.
///
/// The argument arrives as an arbitrary JSON object, so every shape is validated here
/// rather than at SQL-assembly time: an unusable filter must fail the call, not silently
/// widen it.
pub fn parse_metadata_filters(raw: &Value) -> Result<Vec<FieldFilter>> {
    let Some(object) = raw.as_object() else {
        // A non-object value filters nothing, which is how the reference treats it.
        return Ok(Vec::new());
    };
    let mut filters = Vec::with_capacity(object.len());
    for (key, value) in object {
        // `note_type` is the caller-friendly spelling of the frontmatter key `type`.
        let key = if key == "note_type" { "type" } else { key };
        if !is_identifier_key(key) {
            return Err(Error::InvalidArgument {
                message: format!(
                    "invalid metadata filter key '{key}': a frontmatter key must be \
                     alphanumeric or '_'"
                ),
            });
        }
        filters.push(FieldFilter {
            key: key.to_owned(),
            ops: parse_field_ops(key, value)?,
        });
    }
    Ok(filters)
}

/// Parse one field's value: a bare scalar is an exact match, an object is operators.
fn parse_field_ops(key: &str, value: &Value) -> Result<Vec<FilterOp>> {
    let Some(object) = value.as_object() else {
        // A bare scalar, array or null: exact match, as before.
        return Ok(vec![FilterOp::Eq(value.clone())]);
    };
    // An empty object cannot mean "match everything" without silently widening the
    // caller's filter, so it is refused like an unknown operator.
    if object.is_empty() {
        return Err(Error::InvalidArgument {
            message: format!("metadata filter for '{key}' has no operators"),
        });
    }
    let mut ops = Vec::with_capacity(object.len());
    for (raw_op, operand) in object {
        let op = normalize_operator(raw_op).ok_or_else(|| Error::InvalidArgument {
            message: format!(
                "unknown metadata filter operator '{raw_op}'. Valid operators: {}",
                OPERATORS.join(", ")
            ),
        })?;
        ops.push(build_op(op, operand)?);
    }
    Ok(ops)
}

/// Accept both `$gte` and the bare `gte` spelling (#1676).
fn normalize_operator(raw: &str) -> Option<&'static str> {
    let bare = raw.strip_prefix('$').unwrap_or(raw);
    match bare {
        "eq" => Some("$eq"),
        "ne" => Some("$ne"),
        "gt" => Some("$gt"),
        "gte" => Some("$gte"),
        "lt" => Some("$lt"),
        "lte" => Some("$lte"),
        "in" => Some("$in"),
        "contains" => Some("$contains"),
        "exists" => Some("$exists"),
        _ => None,
    }
}

fn build_op(op: &'static str, operand: &Value) -> Result<FilterOp> {
    match op {
        "$eq" => Ok(FilterOp::Eq(operand.clone())),
        "$ne" => Ok(FilterOp::Ne(operand.clone())),
        "$gt" => Ok(FilterOp::Gt(operand.clone())),
        "$gte" => Ok(FilterOp::Gte(operand.clone())),
        "$lt" => Ok(FilterOp::Lt(operand.clone())),
        "$lte" => Ok(FilterOp::Lte(operand.clone())),
        "$in" => match operand.as_array() {
            Some(values) => Ok(FilterOp::In(values.clone())),
            None => Err(Error::InvalidArgument {
                message: "$in requires a list of values".to_owned(),
            }),
        },
        "$contains" => Ok(FilterOp::Contains(operand.clone())),
        "$exists" => match operand.as_bool() {
            Some(present) => Ok(FilterOp::Exists(present)),
            None => Err(Error::InvalidArgument {
                message: "$exists requires true or false".to_owned(),
            }),
        },
        other => Err(Error::InvalidArgument {
            message: format!("unknown metadata filter operator '{other}'"),
        }),
    }
}

/// Whether `key` can be interpolated into a `json_extract` path.
fn is_identifier_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One SQL predicate plus its bind parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledFilter {
    /// SQL fragment referencing `entity_metadata`.
    pub sql: String,
    /// Bind values, in placeholder order.
    pub params: Vec<SqlValue>,
}

/// Compile one field filter into SQL over an `entity_metadata` column.
///
/// Every comparison is applied to the same JSON path, and the caller ANDs the returned
/// fragments together with its other predicates. The path is bound as a parameter, so
/// the field name never reaches the SQL text.
pub fn compile_field_filter(filter: &FieldFilter) -> Result<CompiledFilter> {
    let path = format!("$.{}", filter.key);
    let mut clauses = Vec::with_capacity(filter.ops.len());
    let mut params: Vec<SqlValue> = Vec::new();
    for op in &filter.ops {
        let clause = match op {
            FilterOp::Eq(value) => {
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "json_extract(entity_metadata, ?) = ?".to_owned()
            }
            FilterOp::Ne(value) => {
                // A missing key is not "not equal": only rows that declare the field are
                // considered, so the null test is explicit.
                params.push(SqlValue::Text(path.clone()));
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "(json_extract(entity_metadata, ?) IS NOT NULL \
                 AND json_extract(entity_metadata, ?) <> ?)"
                    .to_owned()
            }
            FilterOp::Gt(value) => {
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "json_extract(entity_metadata, ?) > ?".to_owned()
            }
            FilterOp::Gte(value) => {
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "json_extract(entity_metadata, ?) >= ?".to_owned()
            }
            FilterOp::Lt(value) => {
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "json_extract(entity_metadata, ?) < ?".to_owned()
            }
            FilterOp::Lte(value) => {
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "json_extract(entity_metadata, ?) <= ?".to_owned()
            }
            FilterOp::In(values) => {
                if values.is_empty() {
                    // An empty `$in` matches nothing, which is the honest reading;
                    // dropping the clause would answer a wider question than asked.
                    "0".to_owned()
                } else {
                    let placeholders = vec!["?"; values.len()].join(", ");
                    params.push(SqlValue::Text(path.clone()));
                    for value in values {
                        params.push(json_operand(value));
                    }
                    format!("json_extract(entity_metadata, ?) IN ({placeholders})")
                }
            }
            FilterOp::Contains(value) => {
                params.push(SqlValue::Text(path.clone()));
                params.push(json_operand(value));
                "EXISTS (SELECT 1 FROM json_each(entity_metadata, ?) \
                 WHERE json_each.value = ?)"
                    .to_owned()
            }
            FilterOp::Exists(present) => {
                params.push(SqlValue::Text(path.clone()));
                if *present {
                    "json_extract(entity_metadata, ?) IS NOT NULL".to_owned()
                } else {
                    "json_extract(entity_metadata, ?) IS NULL".to_owned()
                }
            }
        };
        clauses.push(clause);
    }
    Ok(CompiledFilter {
        sql: clauses.join(" AND "),
        params,
    })
}

/// Convert a JSON operand to the SQLite storage class `json_extract` compares against.
///
/// `json_extract` yields SQLite's own classes: an integer, a real, a string, or NULL.
/// Binding a JSON number as text would compare `2` against `'2'` and silently miss.
fn json_operand(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(flag) => SqlValue::Integer(i64::from(*flag)),
        Value::Number(number) => number
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| number.as_f64().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null),
        Value::String(text) => SqlValue::Text(text.clone()),
        // Arrays and objects compare as their JSON text, which is what a caller storing
        // a structured value in frontmatter expects an exact match against.
        other => SqlValue::Text(other.to_string()),
    }
}

/// The legacy `key -> rendered string` view, for callers that only do exact matches.
///
/// Kept so the existing `TextSearchOptions::metadata_filters` callers (the CLI, the
/// hook brief) do not have to learn the new type at once.
#[must_use]
pub fn exact_only(filters: &BTreeMap<String, String>) -> Vec<FieldFilter> {
    filters
        .iter()
        .map(|(key, value)| FieldFilter {
            key: if key == "note_type" {
                "type".to_owned()
            } else {
                key.clone()
            },
            ops: vec![FilterOp::Eq(Value::String(value.clone()))],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filters(raw: Value) -> Vec<FieldFilter> {
        parse_metadata_filters(&raw).expect("filters")
    }

    #[test]
    fn a_bare_scalar_is_an_exact_match() {
        let parsed = filters(json!({ "status": "active" }));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, "status");
        assert_eq!(parsed[0].ops, vec![FilterOp::Eq(json!("active"))]);
    }

    #[test]
    fn note_type_is_an_alias_for_the_type_key() {
        let parsed = filters(json!({ "note_type": "person" }));
        assert_eq!(parsed[0].key, "type");
    }

    #[test]
    fn several_operators_apply_to_one_field() {
        let parsed = filters(json!({ "priority": { "$gte": 2, "$lte": 8 } }));
        assert_eq!(parsed[0].ops.len(), 2);
        assert!(
            parsed[0].ops.contains(&FilterOp::Gte(json!(2))),
            "the lower bound must be recorded"
        );
        assert!(
            parsed[0].ops.contains(&FilterOp::Lte(json!(8))),
            "the upper bound must be recorded"
        );
    }

    #[test]
    fn bare_operator_names_are_aliases() {
        let parsed = filters(json!({ "priority": { "gte": 2 } }));
        assert_eq!(parsed[0].ops, vec![FilterOp::Gte(json!(2))]);
    }

    #[test]
    fn contains_and_exists_and_in_parse() {
        let parsed = filters(json!({
            "tags": { "$contains": "rust" },
            "reviewed": { "$exists": true },
            "status": { "$in": ["active", "done"] }
        }));
        let by_key = |key: &str| parsed.iter().find(|f| f.key == key).expect(key);
        assert_eq!(by_key("tags").ops, vec![FilterOp::Contains(json!("rust"))]);
        assert_eq!(by_key("reviewed").ops, vec![FilterOp::Exists(true)]);
        assert_eq!(
            by_key("status").ops,
            vec![FilterOp::In(vec![json!("active"), json!("done")])]
        );
    }

    #[test]
    fn an_unknown_operator_lists_the_valid_ones() {
        let error = parse_metadata_filters(&json!({ "priority": { "$between": 2 } }))
            .expect_err("unknown operator");
        let message = error.to_string();
        assert!(
            message.contains("unknown metadata filter operator"),
            "{message}"
        );
        assert!(
            message.contains("$gte"),
            "the message must list the valid ones: {message}"
        );
    }

    #[test]
    fn an_empty_operator_object_is_refused() {
        let error = parse_metadata_filters(&json!({ "priority": {} })).expect_err("no operators");
        assert!(error.to_string().contains("has no operators"), "{error}");
    }

    #[test]
    fn an_injection_shaped_key_is_refused() {
        let error =
            parse_metadata_filters(&json!({ "status' OR 1=1 --": "x" })).expect_err("unsafe key");
        assert!(
            error.to_string().contains("invalid metadata filter key"),
            "{error}"
        );
    }

    #[test]
    fn a_non_object_argument_filters_nothing() {
        assert!(
            parse_metadata_filters(&json!("nope"))
                .expect("ok")
                .is_empty()
        );
        assert!(
            parse_metadata_filters(&Value::Null).expect("ok").is_empty(),
            "a null filter object filters nothing"
        );
    }

    #[test]
    fn an_in_operator_requires_a_list() {
        let error =
            parse_metadata_filters(&json!({ "status": { "$in": "active" } })).expect_err("bad in");
        assert!(error.to_string().contains("$in requires a list"), "{error}");
    }

    #[test]
    fn exists_requires_a_boolean() {
        let error = parse_metadata_filters(&json!({ "reviewed": { "$exists": "yes" } }))
            .expect_err("bad exists");
        assert!(
            error.to_string().contains("$exists requires true or false"),
            "{error}"
        );
    }

    #[test]
    fn compiling_an_exact_match_binds_the_path_then_the_value() {
        let parsed = filters(json!({ "status": "active" }));
        let compiled = compile_field_filter(&parsed[0]).expect("compiled");
        assert_eq!(compiled.sql, "json_extract(entity_metadata, ?) = ?");
        assert_eq!(
            compiled.params,
            vec![
                SqlValue::Text("$.status".to_owned()),
                SqlValue::Text("active".to_owned())
            ]
        );
    }

    #[test]
    fn compiling_two_operators_ands_them() {
        let parsed = filters(json!({ "priority": { "$gte": 2, "$lte": 8 } }));
        let compiled = compile_field_filter(&parsed[0]).expect("compiled");
        assert_eq!(
            compiled.sql,
            "json_extract(entity_metadata, ?) >= ? AND json_extract(entity_metadata, ?) <= ?"
        );
        assert_eq!(
            compiled.params,
            vec![
                SqlValue::Text("$.priority".to_owned()),
                SqlValue::Integer(2),
                SqlValue::Text("$.priority".to_owned()),
                SqlValue::Integer(8)
            ]
        );
    }

    #[test]
    fn an_empty_in_matches_nothing() {
        let parsed = filters(json!({ "status": { "$in": [] } }));
        let compiled = compile_field_filter(&parsed[0]).expect("compiled");
        assert_eq!(compiled.sql, "0");
        assert!(compiled.params.is_empty(), "an empty $in binds no values");
    }

    #[test]
    fn exact_only_renders_the_legacy_view() {
        let mut legacy = BTreeMap::new();
        legacy.insert("note_type".to_owned(), "person".to_owned());
        let parsed = exact_only(&legacy);
        assert_eq!(parsed[0].key, "type");
        assert_eq!(parsed[0].ops, vec![FilterOp::Eq(json!("person"))]);
    }
}
