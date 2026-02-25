//! Shared OpenAI-compatible wire format types and helpers.
//!
//! Used by [`openai`] and [`openrouter`] to avoid duplicating the request/response
//! struct definitions and message-conversion logic.

use crate::providers::traits::{ChatMessage, ToolCall as ProviderToolCall};
use serde::{Deserialize, Serialize};

/// OpenAI-compatible chat completion request body.
#[derive(Debug, Serialize)]
pub struct NativeChatRequest {
    pub model: String,
    pub messages: Vec<NativeMessage>,
    pub temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
}

/// A single message in an OpenAI-compatible request.
#[derive(Debug, Serialize)]
pub struct NativeMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<NativeToolCall>>,
}

/// Tool specification sent to the model.
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeToolSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: NativeToolFunctionSpec,
}

/// Function metadata within a tool specification.
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeToolFunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool call returned by the model.
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub function: NativeFunctionCall,
}

/// Function call within a tool call.
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeFunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Convert provider [`ChatMessage`]s to OpenAI-compatible wire messages.
///
/// Handles assistant tool-call messages (JSON-encoded `{"tool_calls": [...]}` content)
/// and tool-result messages (`{"tool_call_id": "...", "content": "..."}` content).
pub fn convert_messages(messages: &[ChatMessage]) -> Vec<NativeMessage> {
    messages
        .iter()
        .map(|m| {
            if m.role == "assistant" {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                    if let Some(tool_calls_value) = value.get("tool_calls") {
                        if let Ok(parsed_calls) =
                            serde_json::from_value::<Vec<ProviderToolCall>>(
                                tool_calls_value.clone(),
                            )
                        {
                            let tool_calls = parsed_calls
                                .into_iter()
                                .map(|tc| NativeToolCall {
                                    id: Some(tc.id),
                                    kind: Some("function".to_string()),
                                    function: NativeFunctionCall {
                                        name: tc.name,
                                        arguments: tc.arguments,
                                    },
                                })
                                .collect::<Vec<_>>();
                            let content = value
                                .get("content")
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string);
                            return NativeMessage {
                                role: "assistant".to_string(),
                                content,
                                tool_call_id: None,
                                tool_calls: Some(tool_calls),
                            };
                        }
                    }
                }
            }

            if m.role == "tool" {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                    let tool_call_id = value
                        .get("tool_call_id")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return NativeMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id,
                        tool_calls: None,
                    };
                }
            }

            NativeMessage {
                role: m.role.clone(),
                content: Some(m.content.clone()),
                tool_call_id: None,
                tool_calls: None,
            }
        })
        .collect()
}
