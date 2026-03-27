use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionIntent {
    pub tool_name: String,
    pub operation: String,
    pub parameters: Value,
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    ExactMatch {
        field: String,
        expected: Value,
    },
    Contains {
        field: String,
        substring: String,
    },
    Schema {
        field: String,
        schema: Value,
    },
    Range {
        field: String,
        min: Option<f64>,
        max: Option<f64>,
    },
    NonEmpty {
        field: String,
    },
    Custom {
        name: String,
        params: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionOutcome {
    pub tool_name: String,
    pub operation: String,
    pub result: Value,
    pub metadata: Option<ProvenanceMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvenanceMetadata {
    pub citations: Vec<Citation>,
    pub reasoning_steps: Vec<String>,
    pub sources_consulted: u32,
    pub provider_metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Citation {
    pub url: Option<String>,
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationResult {
    pub status: VerificationStatus,
    pub mode: VerificationMode,
    pub strategy: String,
    pub duration_ms: u64,
    pub details: Vec<VerificationDetail>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Verified,
    Failed,
    Inconclusive,
    Skipped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    Mutation,
    Output,
    Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationDetail {
    pub check: String,
    pub passed: bool,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub diff: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("verifier for operation `{operation}` is already registered")]
    DuplicateOperation { operation: String },
}

pub trait Verifier: Send + Sync {
    fn handles(&self) -> &[&str];

    fn verify_active(&self, intent: &ActionIntent, outcome: &ActionOutcome) -> VerificationResult;

    fn verify_passive(&self, outcome: &ActionOutcome) -> VerificationResult;
}

#[derive(Default)]
pub struct VerifierRegistry {
    verifiers: HashMap<String, Arc<dyn Verifier>>,
}

impl VerifierRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<V>(&mut self, verifier: V) -> Result<(), RegistryError>
    where
        V: Verifier + 'static,
    {
        let verifier: Arc<dyn Verifier> = Arc::new(verifier);
        let operations = verifier
            .handles()
            .iter()
            .map(|op| (*op).to_string())
            .collect::<Vec<_>>();
        for operation in &operations {
            if self.verifiers.contains_key(operation) {
                return Err(RegistryError::DuplicateOperation {
                    operation: operation.clone(),
                });
            }
        }

        for operation in operations {
            self.verifiers.insert(operation, Arc::clone(&verifier));
        }

        Ok(())
    }

    pub fn verify_active(
        &self,
        intent: &ActionIntent,
        outcome: &ActionOutcome,
    ) -> VerificationResult {
        self.verifiers
            .get(intent.operation.as_str())
            .map(|verifier| verifier.verify_active(intent, outcome))
            .unwrap_or_else(|| skipped_result(VerificationMode::Mutation, &intent.operation))
    }

    pub fn verify_passive(&self, outcome: &ActionOutcome) -> VerificationResult {
        self.verifiers
            .get(outcome.operation.as_str())
            .map(|verifier| verifier.verify_passive(outcome))
            .unwrap_or_else(|| skipped_result(VerificationMode::Provenance, &outcome.operation))
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileVerifier;

impl FileVerifier {
    pub fn new() -> Self {
        Self
    }
}

impl Verifier for FileVerifier {
    fn handles(&self) -> &[&str] {
        static HANDLES: &[&str] = &["write_file", "edit_file"];
        HANDLES
    }

    fn verify_active(&self, intent: &ActionIntent, outcome: &ActionOutcome) -> VerificationResult {
        let started = Instant::now();
        let path = outcome
            .result
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| intent.parameters.get("path").and_then(Value::as_str))
            .map(PathBuf::from);

        let expected = intent
            .parameters
            .get("content")
            .or_else(|| intent.parameters.get("expected_content"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                intent
                    .constraints
                    .iter()
                    .find_map(|constraint| match constraint {
                        Constraint::ExactMatch { field, expected }
                            if field == "content" || field == "contents" =>
                        {
                            expected.as_str().map(ToOwned::to_owned)
                        }
                        _ => None,
                    })
            });

        let mut details = Vec::new();
        let status = match (path, expected) {
            (Some(path), Some(expected)) => match fs::read_to_string(&path) {
                Ok(actual) => {
                    let diff = if actual == expected {
                        None
                    } else {
                        Some(
                            TextDiff::from_lines(&expected, &actual)
                                .unified_diff()
                                .header("expected", "actual")
                                .to_string(),
                        )
                    };
                    details.push(VerificationDetail {
                        check: "file_contents_match".to_string(),
                        passed: diff.is_none(),
                        expected: Some(expected),
                        actual: Some(actual),
                        diff,
                    });
                    summarize_status(&details)
                }
                Err(error) => {
                    details.push(VerificationDetail {
                        check: "file_read_back".to_string(),
                        passed: false,
                        expected: Some("readable file".to_string()),
                        actual: Some(error.to_string()),
                        diff: None,
                    });
                    VerificationStatus::Inconclusive
                }
            },
            _ => {
                details.push(VerificationDetail {
                    check: "file_verifier_inputs".to_string(),
                    passed: false,
                    expected: Some(
                        "intent.parameters.path and intent.parameters.content".to_string(),
                    ),
                    actual: Some("missing path or expected content".to_string()),
                    diff: None,
                });
                VerificationStatus::Inconclusive
            }
        };

        build_result(
            "file_verifier",
            VerificationMode::Mutation,
            started,
            status,
            details,
        )
    }

    fn verify_passive(&self, outcome: &ActionOutcome) -> VerificationResult {
        inconclusive_result(
            "file_verifier",
            VerificationMode::Provenance,
            "passive verification is not supported for file mutations",
            Some(outcome.operation.clone()),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct JsonVerifier;

impl JsonVerifier {
    pub fn new() -> Self {
        Self
    }
}

impl Verifier for JsonVerifier {
    fn handles(&self) -> &[&str] {
        static HANDLES: &[&str] = &["json_output", "generate_json"];
        HANDLES
    }

    fn verify_active(&self, intent: &ActionIntent, outcome: &ActionOutcome) -> VerificationResult {
        let started = Instant::now();
        let mut details = Vec::new();
        let mut status = VerificationStatus::Verified;

        for constraint in &intent.constraints {
            match apply_constraint(constraint, &outcome.result) {
                ConstraintOutcome::Detail(detail) => {
                    if !detail.passed {
                        status = VerificationStatus::Failed;
                    }
                    details.push(detail);
                }
                ConstraintOutcome::Inconclusive(detail) => {
                    if status != VerificationStatus::Failed {
                        status = VerificationStatus::Inconclusive;
                    }
                    details.push(detail);
                }
            }
        }

        build_result(
            "json_verifier",
            VerificationMode::Output,
            started,
            status,
            details,
        )
    }

    fn verify_passive(&self, outcome: &ActionOutcome) -> VerificationResult {
        let started = Instant::now();
        let mut details = Vec::new();
        let status = match &outcome.metadata {
            Some(metadata) => {
                details.push(VerificationDetail {
                    check: "citations_present".to_string(),
                    passed: !metadata.citations.is_empty(),
                    expected: Some("one or more citations".to_string()),
                    actual: Some(metadata.citations.len().to_string()),
                    diff: None,
                });
                details.push(VerificationDetail {
                    check: "sources_consulted".to_string(),
                    passed: metadata.sources_consulted > 0,
                    expected: Some("sources_consulted > 0".to_string()),
                    actual: Some(metadata.sources_consulted.to_string()),
                    diff: None,
                });
                summarize_status(&details)
            }
            None => {
                details.push(VerificationDetail {
                    check: "provenance_metadata".to_string(),
                    passed: false,
                    expected: Some("metadata present".to_string()),
                    actual: Some("metadata missing".to_string()),
                    diff: None,
                });
                VerificationStatus::Inconclusive
            }
        };

        build_result(
            "json_verifier",
            VerificationMode::Provenance,
            started,
            status,
            details,
        )
    }
}

#[cfg(feature = "http")]
#[derive(Debug, Clone)]
pub struct HttpVerifier {
    client: reqwest::blocking::Client,
}

#[cfg(feature = "http")]
impl Default for HttpVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "http")]
impl HttpVerifier {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
        }
    }
}

#[cfg(feature = "http")]
impl Verifier for HttpVerifier {
    fn handles(&self) -> &[&str] {
        static HANDLES: &[&str] = &["generate_image", "generate_video", "artifact_url"];
        HANDLES
    }

    fn verify_active(&self, intent: &ActionIntent, outcome: &ActionOutcome) -> VerificationResult {
        let started = Instant::now();
        let mut details = Vec::new();
        let url = resolve_field(&outcome.result, "url")
            .and_then(Value::as_str)
            .or_else(|| resolve_field(&outcome.result, "artifact.url").and_then(Value::as_str));

        let status = match url {
            Some(url) => match self.client.head(url).send() {
                Ok(response) => {
                    let status_ok = response.status().is_success();
                    let content_type = response
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned);
                    let content_length = response
                        .headers()
                        .get(reqwest::header::CONTENT_LENGTH)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok());

                    details.push(VerificationDetail {
                        check: "http_reachable".to_string(),
                        passed: status_ok,
                        expected: Some("2xx response".to_string()),
                        actual: Some(response.status().to_string()),
                        diff: None,
                    });

                    let expected_content_type =
                        intent
                            .constraints
                            .iter()
                            .find_map(|constraint| match constraint {
                                Constraint::ExactMatch { field, expected }
                                    if field == "content_type" =>
                                {
                                    expected.as_str().map(ToOwned::to_owned)
                                }
                                _ => None,
                            });
                    if let Some(expected_content_type) = expected_content_type {
                        details.push(VerificationDetail {
                            check: "content_type".to_string(),
                            passed: content_type.as_deref() == Some(expected_content_type.as_str()),
                            expected: Some(expected_content_type),
                            actual: content_type,
                            diff: None,
                        });
                    }

                    details.push(VerificationDetail {
                        check: "content_length_non_zero".to_string(),
                        passed: content_length.unwrap_or(0) > 0,
                        expected: Some("> 0".to_string()),
                        actual: content_length.map(|value| value.to_string()),
                        diff: None,
                    });

                    summarize_status(&details)
                }
                Err(error) => {
                    details.push(VerificationDetail {
                        check: "http_head".to_string(),
                        passed: false,
                        expected: Some("reachable URL".to_string()),
                        actual: Some(error.to_string()),
                        diff: None,
                    });
                    VerificationStatus::Inconclusive
                }
            },
            None => {
                details.push(VerificationDetail {
                    check: "artifact_url".to_string(),
                    passed: false,
                    expected: Some("result.url or result.artifact.url".to_string()),
                    actual: Some("missing URL".to_string()),
                    diff: None,
                });
                VerificationStatus::Inconclusive
            }
        };

        build_result(
            "http_verifier",
            VerificationMode::Output,
            started,
            status,
            details,
        )
    }

    fn verify_passive(&self, outcome: &ActionOutcome) -> VerificationResult {
        inconclusive_result(
            "http_verifier",
            VerificationMode::Provenance,
            "passive verification is not supported for HTTP artifacts",
            Some(outcome.operation.clone()),
        )
    }
}

fn build_result(
    strategy: &str,
    mode: VerificationMode,
    started: Instant,
    status: VerificationStatus,
    details: Vec<VerificationDetail>,
) -> VerificationResult {
    VerificationResult {
        status,
        mode,
        strategy: strategy.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        details,
    }
}

fn skipped_result(mode: VerificationMode, operation: &str) -> VerificationResult {
    VerificationResult {
        status: VerificationStatus::Skipped,
        mode,
        strategy: "registry".to_string(),
        duration_ms: 0,
        details: vec![VerificationDetail {
            check: "verifier_registered".to_string(),
            passed: false,
            expected: Some("registered verifier".to_string()),
            actual: Some(format!("no verifier for operation `{operation}`")),
            diff: None,
        }],
    }
}

fn inconclusive_result(
    strategy: &str,
    mode: VerificationMode,
    reason: &str,
    actual: Option<String>,
) -> VerificationResult {
    VerificationResult {
        status: VerificationStatus::Inconclusive,
        mode,
        strategy: strategy.to_string(),
        duration_ms: 0,
        details: vec![VerificationDetail {
            check: "verification_supported".to_string(),
            passed: false,
            expected: Some(reason.to_string()),
            actual,
            diff: None,
        }],
    }
}

fn summarize_status(details: &[VerificationDetail]) -> VerificationStatus {
    if details.iter().any(|detail| !detail.passed) {
        VerificationStatus::Failed
    } else {
        VerificationStatus::Verified
    }
}

enum ConstraintOutcome {
    Detail(VerificationDetail),
    Inconclusive(VerificationDetail),
}

fn apply_constraint(constraint: &Constraint, root: &Value) -> ConstraintOutcome {
    match constraint {
        Constraint::ExactMatch { field, expected } => match resolve_field(root, field) {
            Some(actual) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("exact_match:{field}"),
                passed: actual == expected,
                expected: Some(expected.to_string()),
                actual: Some(actual.to_string()),
                diff: None,
            }),
            None => missing_field(field),
        },
        Constraint::Contains { field, substring } => match resolve_field(root, field) {
            Some(Value::String(actual)) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("contains:{field}"),
                passed: actual.contains(substring),
                expected: Some(substring.clone()),
                actual: Some(actual.clone()),
                diff: None,
            }),
            Some(actual) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("contains:{field}"),
                passed: false,
                expected: Some("string field".to_string()),
                actual: Some(actual.to_string()),
                diff: None,
            }),
            None => missing_field(field),
        },
        Constraint::Schema { field, schema } => match resolve_field(root, field) {
            Some(actual) => {
                let errors = validate_schema(schema, actual, field);
                ConstraintOutcome::Detail(VerificationDetail {
                    check: format!("schema:{field}"),
                    passed: errors.is_empty(),
                    expected: Some("schema-compliant value".to_string()),
                    actual: if errors.is_empty() {
                        Some("valid".to_string())
                    } else {
                        Some(errors.join("; "))
                    },
                    diff: None,
                })
            }
            None => missing_field(field),
        },
        Constraint::Range { field, min, max } => {
            match resolve_field(root, field).and_then(Value::as_f64) {
                Some(actual) => {
                    let min_ok = min.map(|value| actual >= value).unwrap_or(true);
                    let max_ok = max.map(|value| actual <= value).unwrap_or(true);
                    ConstraintOutcome::Detail(VerificationDetail {
                        check: format!("range:{field}"),
                        passed: min_ok && max_ok,
                        expected: Some(format!("min={min:?}, max={max:?}")),
                        actual: Some(actual.to_string()),
                        diff: None,
                    })
                }
                None => missing_or_wrong_type(field, "number"),
            }
        }
        Constraint::NonEmpty { field } => match resolve_field(root, field) {
            Some(Value::String(value)) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("non_empty:{field}"),
                passed: !value.is_empty(),
                expected: Some("non-empty string".to_string()),
                actual: Some(value.clone()),
                diff: None,
            }),
            Some(Value::Array(values)) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("non_empty:{field}"),
                passed: !values.is_empty(),
                expected: Some("non-empty array".to_string()),
                actual: Some(values.len().to_string()),
                diff: None,
            }),
            Some(Value::Object(values)) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("non_empty:{field}"),
                passed: !values.is_empty(),
                expected: Some("non-empty object".to_string()),
                actual: Some(values.len().to_string()),
                diff: None,
            }),
            Some(other) => ConstraintOutcome::Detail(VerificationDetail {
                check: format!("non_empty:{field}"),
                passed: false,
                expected: Some("string, array, or object".to_string()),
                actual: Some(other.to_string()),
                diff: None,
            }),
            None => missing_field(field),
        },
        Constraint::Custom { name, .. } => ConstraintOutcome::Inconclusive(VerificationDetail {
            check: format!("custom:{name}"),
            passed: false,
            expected: Some("consumer-defined verifier".to_string()),
            actual: Some("custom constraints are not handled by JsonVerifier".to_string()),
            diff: None,
        }),
    }
}

fn missing_field(field: &str) -> ConstraintOutcome {
    ConstraintOutcome::Detail(VerificationDetail {
        check: format!("field_present:{field}"),
        passed: false,
        expected: Some("field present".to_string()),
        actual: Some("missing".to_string()),
        diff: None,
    })
}

fn missing_or_wrong_type(field: &str, expected_type: &str) -> ConstraintOutcome {
    ConstraintOutcome::Detail(VerificationDetail {
        check: format!("type:{field}"),
        passed: false,
        expected: Some(expected_type.to_string()),
        actual: Some("missing or wrong type".to_string()),
        diff: None,
    })
}

fn resolve_field<'a>(root: &'a Value, field: &str) -> Option<&'a Value> {
    if field.is_empty() || field == "$" {
        return Some(root);
    }

    field
        .trim_start_matches("$.")
        .split('.')
        .try_fold(root, |value, segment| match value {
            Value::Object(map) => map.get(segment),
            Value::Array(items) => segment
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index)),
            _ => None,
        })
}

fn validate_schema(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    let mut errors = Vec::new();

    if let Some(type_name) = schema.get("type").and_then(Value::as_str) {
        let type_matches = matches_json_type(type_name, value);
        if !type_matches {
            errors.push(format!("{path}: expected type `{type_name}`"));
            return errors;
        }
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        if let Value::Object(object) = value {
            for required_field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(required_field) {
                    errors.push(format!("{path}: missing required field `{required_field}`"));
                }
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        if let Value::Object(object) = value {
            for (property, property_schema) in properties {
                if let Some(property_value) = object.get(property) {
                    errors.extend(validate_schema(
                        property_schema,
                        property_value,
                        &format!("{path}.{property}"),
                    ));
                }
            }
        }
    }

    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        if let (Some(properties), Value::Object(object)) =
            (schema.get("properties").and_then(Value::as_object), value)
        {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    errors.push(format!("{path}: unexpected field `{key}`"));
                }
            }
        }
    }

    if let Some(items_schema) = schema.get("items") {
        if let Value::Array(items) = value {
            for (index, item) in items.iter().enumerate() {
                errors.extend(validate_schema(
                    items_schema,
                    item,
                    &format!("{path}[{index}]"),
                ));
            }
        }
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|candidate| candidate == value) {
            errors.push(format!("{path}: value not present in enum"));
        }
    }

    errors
}

fn matches_json_type(type_name: &str, value: &Value) -> bool {
    match type_name {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(feature = "http")]
    use std::io::{Read, Write};
    #[cfg(feature = "http")]
    use std::net::TcpListener;
    #[cfg(feature = "http")]
    use std::thread;

    #[test]
    fn result_types_round_trip_through_serde() {
        let result = VerificationResult {
            status: VerificationStatus::Verified,
            mode: VerificationMode::Provenance,
            strategy: "json_verifier".to_string(),
            duration_ms: 12,
            details: vec![VerificationDetail {
                check: "citations_present".to_string(),
                passed: true,
                expected: Some("1".to_string()),
                actual: Some("1".to_string()),
                diff: None,
            }],
        };

        let encoded = serde_json::to_string(&result).unwrap();
        let decoded: VerificationResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, result);

        let metadata = ProvenanceMetadata {
            citations: vec![Citation {
                url: Some("https://example.com".to_string()),
                title: Some("Example".to_string()),
                snippet: Some("snippet".to_string()),
                domain: Some("example.com".to_string()),
            }],
            reasoning_steps: vec!["step".to_string()],
            sources_consulted: 1,
            provider_metadata: json!({"provider":"xai"}),
        };
        let encoded = serde_json::to_string(&metadata).unwrap();
        let decoded: ProvenanceMetadata = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, metadata);
    }

    #[test]
    fn registry_skips_unknown_operations() {
        let registry = VerifierRegistry::new();
        let result = registry.verify_active(
            &ActionIntent {
                tool_name: "fs".to_string(),
                operation: "unknown".to_string(),
                parameters: json!({}),
                constraints: vec![],
            },
            &ActionOutcome {
                tool_name: "fs".to_string(),
                operation: "unknown".to_string(),
                result: json!({}),
                metadata: None,
            },
        );

        assert_eq!(result.status, VerificationStatus::Skipped);
        assert_eq!(result.mode, VerificationMode::Mutation);
    }

    #[test]
    fn registry_rejects_duplicate_operations() {
        let mut registry = VerifierRegistry::new();
        registry.register(FileVerifier::new()).unwrap();
        let error = registry.register(FileVerifier::new()).unwrap_err();
        assert_eq!(
            error,
            RegistryError::DuplicateOperation {
                operation: "write_file".to_string()
            }
        );
    }

    #[test]
    fn file_verifier_passes_when_contents_match() {
        let temp_dir = std::env::temp_dir().join(format!("lanyte-verify-{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("match.txt");
        fs::write(&file_path, "hello\nworld\n").unwrap();

        let verifier = FileVerifier::new();
        let result = verifier.verify_active(
            &ActionIntent {
                tool_name: "fs".to_string(),
                operation: "write_file".to_string(),
                parameters: json!({"path": file_path, "content": "hello\nworld\n"}),
                constraints: vec![],
            },
            &ActionOutcome {
                tool_name: "fs".to_string(),
                operation: "write_file".to_string(),
                result: json!({}),
                metadata: None,
            },
        );

        assert_eq!(result.status, VerificationStatus::Verified);
        assert!(result.details[0].diff.is_none());
    }

    #[test]
    fn file_verifier_reports_diff_on_mismatch() {
        let temp_dir = std::env::temp_dir().join(format!("lanyte-verify-{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("mismatch.txt");
        fs::write(&file_path, "hello\nmars\n").unwrap();

        let verifier = FileVerifier::new();
        let result = verifier.verify_active(
            &ActionIntent {
                tool_name: "fs".to_string(),
                operation: "write_file".to_string(),
                parameters: json!({"path": file_path, "content": "hello\nworld\n"}),
                constraints: vec![],
            },
            &ActionOutcome {
                tool_name: "fs".to_string(),
                operation: "write_file".to_string(),
                result: json!({}),
                metadata: None,
            },
        );

        assert_eq!(result.status, VerificationStatus::Failed);
        assert!(result.details[0].diff.as_ref().unwrap().contains("-world"));
        assert!(result.details[0].diff.as_ref().unwrap().contains("+mars"));
    }

    #[test]
    fn json_verifier_validates_constraints_and_schema() {
        let verifier = JsonVerifier::new();
        let result = verifier.verify_active(
            &ActionIntent {
                tool_name: "json".to_string(),
                operation: "json_output".to_string(),
                parameters: json!({}),
                constraints: vec![
                    Constraint::ExactMatch {
                        field: "status".to_string(),
                        expected: json!("ok"),
                    },
                    Constraint::Contains {
                        field: "message".to_string(),
                        substring: "green".to_string(),
                    },
                    Constraint::Range {
                        field: "count".to_string(),
                        min: Some(1.0),
                        max: Some(5.0),
                    },
                    Constraint::NonEmpty {
                        field: "items".to_string(),
                    },
                    Constraint::Schema {
                        field: "$".to_string(),
                        schema: json!({
                            "type": "object",
                            "required": ["status", "items"],
                            "properties": {
                                "status": { "type": "string" },
                                "message": { "type": "string" },
                                "count": { "type": "integer" },
                                "items": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                }
                            },
                            "additionalProperties": false
                        }),
                    },
                ],
            },
            &ActionOutcome {
                tool_name: "json".to_string(),
                operation: "json_output".to_string(),
                result: json!({
                    "status": "ok",
                    "message": "all green",
                    "count": 3,
                    "items": ["a", "b"]
                }),
                metadata: None,
            },
        );

        assert_eq!(result.status, VerificationStatus::Verified);
        assert!(result.details.iter().all(|detail| detail.passed));
    }

    #[test]
    fn json_verifier_reports_schema_mismatch() {
        let verifier = JsonVerifier::new();
        let result = verifier.verify_active(
            &ActionIntent {
                tool_name: "json".to_string(),
                operation: "json_output".to_string(),
                parameters: json!({}),
                constraints: vec![Constraint::Schema {
                    field: "$".to_string(),
                    schema: json!({
                        "type": "object",
                        "required": ["status"],
                        "properties": {
                            "status": { "type": "string" }
                        },
                        "additionalProperties": false
                    }),
                }],
            },
            &ActionOutcome {
                tool_name: "json".to_string(),
                operation: "json_output".to_string(),
                result: json!({"status": 7}),
                metadata: None,
            },
        );

        assert_eq!(result.status, VerificationStatus::Failed);
        assert!(result.details[0]
            .actual
            .as_ref()
            .unwrap()
            .contains("expected type `string`"));
    }

    #[cfg(feature = "http")]
    #[test]
    fn http_verifier_checks_head_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer).unwrap();
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: image/png\r\n",
                "Content-Length: 42\r\n",
                "\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let verifier = HttpVerifier::new();
        let result = verifier.verify_active(
            &ActionIntent {
                tool_name: "image".to_string(),
                operation: "generate_image".to_string(),
                parameters: json!({}),
                constraints: vec![Constraint::ExactMatch {
                    field: "content_type".to_string(),
                    expected: json!("image/png"),
                }],
            },
            &ActionOutcome {
                tool_name: "image".to_string(),
                operation: "generate_image".to_string(),
                result: json!({ "url": format!("http://{address}/image.png") }),
                metadata: None,
            },
        );

        server.join().unwrap();
        assert_eq!(result.status, VerificationStatus::Verified);
    }
}
