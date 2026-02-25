//! Shared OpenAI-compatible wire format types and helpers.
//!
//! Used by [`openai`] and [`openrouter`] to avoid duplicating the request/response
//! struct definitions and message-conversion logic.

use crate::providers::traits::{ChatMessage, ToolCall as ProviderToolCall};
use serde::{Deserialize, Serialize};

const IMAGE_MARKER_PREFIX: &str = "[IMAGE:";

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

/// Message content: either a plain string or an array of content parts (vision).
///
/// OpenAI's API accepts `content` as either `"hello"` or
/// `[{"type":"text","text":"hello"}, {"type":"image_url","image_url":{"url":"data:..."}}]`.
/// The `#[serde(untagged)]` attribute ensures correct serialization for both forms.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum NativeContent {
    /// Plain text content (default for non-vision messages).
    Text(String),
    /// Multipart content with text and image blocks (vision messages).
    Parts(Vec<ContentPart>),
}

impl NativeContent {
    /// Extract the text value if this is a `Text` variant, or `None` for `Parts`.
    pub fn as_text_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            Self::Parts(_) => None,
        }
    }
}

/// A single content part within a multipart vision message.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    /// Text segment.
    #[serde(rename = "text")]
    Text { text: String },
    /// Image reference (base64 data URI or URL).
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

/// Image URL payload within an `image_url` content part.
#[derive(Debug, Serialize)]
pub struct ImageUrl {
    /// The image source — either a data URI (`data:image/png;base64,...`) or HTTPS URL.
    pub url: String,
}

/// A single message in an OpenAI-compatible request.
#[derive(Debug, Serialize)]
pub struct NativeMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<NativeContent>,
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
    convert_messages_inner(messages, false)
}

/// Convert provider [`ChatMessage`]s to OpenAI-compatible wire messages with
/// vision support. User messages containing `[IMAGE:...]` markers are converted
/// to multipart content arrays with text + `image_url` blocks.
pub fn convert_messages_with_vision(messages: &[ChatMessage]) -> Vec<NativeMessage> {
    convert_messages_inner(messages, true)
}

fn convert_messages_inner(messages: &[ChatMessage], vision: bool) -> Vec<NativeMessage> {
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
                                .map(|s| NativeContent::Text(s.to_string()));
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
                        .map(|s| NativeContent::Text(s.to_string()));
                    return NativeMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id,
                        tool_calls: None,
                    };
                }
            }

            // For user messages with vision enabled, convert [IMAGE:] markers
            // to multipart content blocks.
            if vision && m.role == "user" && m.content.contains(IMAGE_MARKER_PREFIX) {
                if let Some(parts_content) = build_vision_content(&m.content) {
                    return NativeMessage {
                        role: m.role.clone(),
                        content: Some(parts_content),
                        tool_call_id: None,
                        tool_calls: None,
                    };
                }
            }

            NativeMessage {
                role: m.role.clone(),
                content: Some(NativeContent::Text(m.content.clone())),
                tool_call_id: None,
                tool_calls: None,
            }
        })
        .collect()
}

/// Parse `[IMAGE:data:...]` markers from message content and build a
/// `NativeContent::Parts` array with text + image_url blocks.
///
/// Returns `None` if no image markers are found.
pub fn build_vision_content(content: &str) -> Option<NativeContent> {
    let mut parts: Vec<ContentPart> = Vec::new();
    let mut cursor = 0usize;
    let mut text_buffer = String::new();
    let mut found_images = false;

    while let Some(rel_start) = content[cursor..].find(IMAGE_MARKER_PREFIX) {
        let start = cursor + rel_start;
        // Accumulate text before the marker
        text_buffer.push_str(&content[cursor..start]);

        let marker_start = start + IMAGE_MARKER_PREFIX.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            // Unclosed marker — treat rest as text
            text_buffer.push_str(&content[start..]);
            cursor = content.len();
            break;
        };

        let end = marker_start + rel_end;
        let image_ref = content[marker_start..end].trim();

        if image_ref.is_empty() {
            // Empty marker — keep as-is
            text_buffer.push_str(&content[start..=end]);
        } else {
            // Flush accumulated text as a text part
            let trimmed = text_buffer.trim();
            if !trimmed.is_empty() {
                parts.push(ContentPart::Text {
                    text: trimmed.to_string(),
                });
            }
            text_buffer.clear();

            // Add image part
            parts.push(ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: image_ref.to_string(),
                },
            });
            found_images = true;
        }

        cursor = end + 1;
    }

    if !found_images {
        return None;
    }

    // Remaining text after last marker
    if cursor < content.len() {
        text_buffer.push_str(&content[cursor..]);
    }
    let trimmed = text_buffer.trim();
    if !trimmed.is_empty() {
        parts.push(ContentPart::Text {
            text: trimmed.to_string(),
        });
    }

    Some(NativeContent::Parts(parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::ChatMessage;

    // ── Existing tests (adapted for NativeContent) ──────────────────

    #[test]
    fn convert_messages_plain_text() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
        ];
        let native = convert_messages(&messages);
        assert_eq!(native.len(), 2);

        // System message
        match &native[0].content {
            Some(NativeContent::Text(t)) => assert_eq!(t, "You are helpful"),
            other => panic!("expected Text, got {other:?}"),
        }

        // User message
        match &native[1].content {
            Some(NativeContent::Text(t)) => assert_eq!(t, "Hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_serializes_text_as_string() {
        let msg = NativeMessage {
            role: "user".to_string(),
            content: Some(NativeContent::Text("hello".to_string())),
            tool_call_id: None,
            tool_calls: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // Should serialize as "content":"hello" (a string, not an array)
        assert!(json.contains(r#""content":"hello""#));
    }

    #[test]
    fn convert_messages_serializes_parts_as_array() {
        let msg = NativeMessage {
            role: "user".to_string(),
            content: Some(NativeContent::Parts(vec![
                ContentPart::Text {
                    text: "Look at this".to_string(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,abc123".to_string(),
                    },
                },
            ])),
            tool_call_id: None,
            tool_calls: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // Should serialize as an array with type tags
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""type":"image_url""#));
        assert!(json.contains(r#""url":"data:image/png;base64,abc123""#));
    }

    // ── Vision conversion tests ─────────────────────────────────────

    #[test]
    fn convert_messages_with_vision_creates_multipart_content() {
        let messages = vec![ChatMessage::user(
            "Process this receipt [IMAGE:data:image/png;base64,abc123]".to_string(),
        )];
        let native = convert_messages_with_vision(&messages);
        assert_eq!(native.len(), 1);

        match &native[0].content {
            Some(NativeContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::Text { text } => assert_eq!(text, "Process this receipt"),
                    other => panic!("expected Text part, got {other:?}"),
                }
                match &parts[1] {
                    ContentPart::ImageUrl { image_url } => {
                        assert_eq!(image_url.url, "data:image/png;base64,abc123");
                    }
                    other => panic!("expected ImageUrl part, got {other:?}"),
                }
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_with_vision_multiple_images() {
        let messages = vec![ChatMessage::user(
            "Compare [IMAGE:data:image/png;base64,img1] and [IMAGE:data:image/jpeg;base64,img2]"
                .to_string(),
        )];
        let native = convert_messages_with_vision(&messages);

        match &native[0].content {
            Some(NativeContent::Parts(parts)) => {
                assert_eq!(parts.len(), 4); // text, image, text("and"), image
                // First text
                assert!(matches!(&parts[0], ContentPart::Text { text } if text == "Compare"));
                // First image
                assert!(
                    matches!(&parts[1], ContentPart::ImageUrl { image_url } if image_url.url.contains("img1"))
                );
                // Middle text
                assert!(matches!(&parts[2], ContentPart::Text { text } if text == "and"));
                // Second image
                assert!(
                    matches!(&parts[3], ContentPart::ImageUrl { image_url } if image_url.url.contains("img2"))
                );
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_with_vision_no_markers_stays_text() {
        let messages = vec![ChatMessage::user("Just a normal message".to_string())];
        let native = convert_messages_with_vision(&messages);

        match &native[0].content {
            Some(NativeContent::Text(t)) => assert_eq!(t, "Just a normal message"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_with_vision_non_user_messages_unchanged() {
        let messages = vec![
            ChatMessage::system("System [IMAGE:data:image/png;base64,ignored]"),
            ChatMessage::assistant("Assistant [IMAGE:data:image/png;base64,ignored]"),
        ];
        let native = convert_messages_with_vision(&messages);

        // System and assistant messages should remain as plain text
        assert!(matches!(&native[0].content, Some(NativeContent::Text(_))));
        assert!(matches!(&native[1].content, Some(NativeContent::Text(_))));
    }

    #[test]
    fn convert_messages_without_vision_ignores_markers() {
        let messages = vec![ChatMessage::user(
            "Receipt [IMAGE:data:image/png;base64,abc]".to_string(),
        )];
        let native = convert_messages(&messages);

        // Without vision flag, markers are kept as plain text
        match &native[0].content {
            Some(NativeContent::Text(t)) => {
                assert!(t.contains("[IMAGE:"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn build_vision_content_image_only() {
        let content = "[IMAGE:data:image/png;base64,abc123]";
        let result = build_vision_content(content).unwrap();

        match result {
            NativeContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                assert!(matches!(&parts[0], ContentPart::ImageUrl { .. }));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn build_vision_content_no_markers_returns_none() {
        assert!(build_vision_content("no markers here").is_none());
    }

    #[test]
    fn build_vision_content_empty_marker_returns_none() {
        assert!(build_vision_content("text [IMAGE:] more text").is_none());
    }
}
