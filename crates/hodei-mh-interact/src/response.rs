//! Answer validation (AC-2): a structured answer must reference ONE valid
//! option (cardinality enforced per request); anything else is rejected
//! before it can reach the agent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::request::InteractionRequest;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("invalid answer: {0}")]
pub struct InvalidAnswer(pub String);

/// A validated answer, ready to be serialized as the ACP response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidAnswer {
    pub json: Value,
}

/// Validate a candidate answer against the request it answers.
pub fn validate(
    request: &InteractionRequest,
    answer: &Value,
) -> Result<ValidAnswer, InvalidAnswer> {
    match request {
        InteractionRequest::Permission { options, .. } => {
            let selected = answer
                .get("outcome")
                .and_then(|o| o.get("optionId"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    InvalidAnswer("answer must select an outcome with optionId".into())
                })?;
            let known = options.iter().any(|o| o.option_id == selected);
            if !known {
                return Err(InvalidAnswer(format!(
                    "selected optionId '{selected}' was not offered"
                )));
            }
            // Exactly one selection: cardinality 1 (AC-2).
            let selections = answer
                .get("outcome")
                .map(|o| o.as_object().map(|m| m.len()).unwrap_or(0))
                .unwrap_or(0);
            if selections != 2 {
                return Err(InvalidAnswer(
                    "permission outcome must be exactly {outcome, optionId}".into(),
                ));
            }
            Ok(ValidAnswer {
                json: answer.clone(),
            })
        }
        InteractionRequest::Form { fields } => {
            let answers = answer.as_object().ok_or_else(|| {
                InvalidAnswer("form answer must be an object field->value".into())
            })?;
            for field in fields {
                let given = answers
                    .get(&field.id)
                    .ok_or_else(|| InvalidAnswer(format!("missing field '{}'", field.id)))?;
                match given {
                    Value::String(one) => {
                        if field.cardinality != 1 {
                            return Err(InvalidAnswer(format!(
                                "field '{}' expects {} values",
                                field.id, field.cardinality
                            )));
                        }
                        if !field.options.contains(one) {
                            return Err(InvalidAnswer(format!(
                                "value '{one}' is not a valid option for '{}'",
                                field.id
                            )));
                        }
                    }
                    Value::Array(many) => {
                        if many.len() != field.cardinality {
                            return Err(InvalidAnswer(format!(
                                "field '{}' expects exactly {} values, got {}",
                                field.id,
                                field.cardinality,
                                many.len()
                            )));
                        }
                        for item in many {
                            let value = item.as_str().ok_or_else(|| {
                                InvalidAnswer(format!("field '{}' options are strings", field.id))
                            })?;
                            if !field.options.contains(&value.to_string()) {
                                return Err(InvalidAnswer(format!(
                                    "value '{value}' is not a valid option for '{}'",
                                    field.id
                                )));
                            }
                        }
                    }
                    _ => {
                        return Err(InvalidAnswer(format!(
                            "field '{}' expects string or array",
                            field.id
                        )));
                    }
                }
            }
            Ok(ValidAnswer {
                json: answer.clone(),
            })
        }
        InteractionRequest::Question { .. } => {
            let text = answer
                .as_str()
                .ok_or_else(|| InvalidAnswer("question answer must be a string".into()))?;
            if text.trim().is_empty() {
                return Err(InvalidAnswer("empty answer is not an answer".into()));
            }
            Ok(ValidAnswer {
                json: answer.clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{FormField, PermissionOption, ToolKind};

    fn permission() -> InteractionRequest {
        InteractionRequest::Permission {
            tool_name: "Write".into(),
            kind: ToolKind::Write,
            options: vec![
                PermissionOption {
                    option_id: "a1".into(),
                    name: "Allow once".into(),
                    kind: "allow_once".into(),
                },
                PermissionOption {
                    option_id: "r1".into(),
                    name: "Reject".into(),
                    kind: "reject_once".into(),
                },
            ],
        }
    }

    #[test]
    fn permission_answer_must_reference_an_offered_option() {
        let ok = validate(
            &permission(),
            &serde_json::json!({
                "outcome": {"outcome": "selected", "optionId": "a1"}
            }),
        );
        assert!(ok.is_ok());

        let unknown = validate(
            &permission(),
            &serde_json::json!({
                "outcome": {"outcome": "selected", "optionId": "made-up"}
            }),
        );
        assert!(unknown.is_err());

        let malformed = validate(&permission(), &serde_json::json!({"outcome": "allow"}));
        assert!(malformed.is_err());
    }

    #[test]
    fn form_answers_honor_cardinality_and_options() {
        let request = InteractionRequest::Form {
            fields: vec![FormField {
                id: "region".into(),
                label: "Region".into(),
                options: vec!["eu".into(), "us".into()],
                cardinality: 1,
            }],
        };
        assert!(validate(&request, &serde_json::json!({"region": "eu"})).is_ok());
        assert!(validate(&request, &serde_json::json!({"region": "ap"})).is_err());
        assert!(validate(&request, &serde_json::json!({"region": ["eu", "us"]})).is_err());
        assert!(validate(&request, &serde_json::json!({})).is_err());
    }

    #[test]
    fn question_answers_must_be_nonempty_strings() {
        let request = InteractionRequest::Question {
            text: "why?".into(),
        };
        assert!(validate(&request, &serde_json::json!("because")).is_ok());
        assert!(validate(&request, &serde_json::json!("  ")).is_err());
        assert!(validate(&request, &serde_json::json!(42)).is_err());
    }
}
