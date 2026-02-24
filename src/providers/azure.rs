use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct AzureOpenAiProvider {
    base_url: String,
    api_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolSpec {
    #[serde(rename = "type")]
    kind: String,
    function: NativeToolFunctionSpec,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    function: NativeFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    choices: Vec<NativeChoice>,
}

#[derive(Debug, Deserialize)]
struct NativeChoice {
    message: NativeResponseMessage,
}

#[derive(Debug, Deserialize)]
struct NativeResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<NativeToolCall>>,
}

impl AzureOpenAiProvider {
    pub fn new(base_url: &str, api_key: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    kind: "function".to_string(),
                    function: NativeToolFunctionSpec {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    },
                })
                .collect()
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> Vec<NativeMessage> {
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

    fn parse_native_response(message: NativeResponseMessage) -> ProviderChatResponse {
        let text = message.content;
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ProviderToolCall {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect::<Vec<_>>();

        ProviderChatResponse { text, tool_calls }
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.azure", 120, 10)
    }

    fn build_url(&self, model: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        // If the URL already looks like a full deployment URL, use it (but ensure api-version is there)
        if base.contains("/openai/deployments/") {
            if base.contains("api-version=") {
                base.to_string()
            } else {
                let separator = if base.contains('?') { "&" } else { "?" };
                format!("{}{}api-version=2024-02-15-preview", base, separator)
            }
        } else {
            // Assume base is the resource URL like https://resource.openai.azure.com/
            format!(
                "{}/openai/deployments/{}/chat/completions?api-version=2024-02-15-preview",
                base, model
            )
        }
    }
}

#[async_trait]
impl Provider for AzureOpenAiProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Azure OpenAI API key not set.")
        })?;

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = ChatRequest {
            messages,
            temperature,
        };

        let url = self.build_url(_model);
        let response = self
            .http_client()
            .post(&url)
            .header("api-key", api_key)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let mut err = super::api_error("Azure", response).await;
            if status == reqwest::StatusCode::NOT_FOUND {
                err = anyhow::anyhow!(
                    "{err}\nAttempted URL: {url}\n\nTip: For Azure OpenAI, the 'model' parameter must match your 'Deployment ID'. \
                    If you haven't created a deployment named '{_model}', please do so in Azure AI Studio or use a model name that matches an existing deployment."
                );
            } else {
                err = anyhow::anyhow!("{err}\nAttempted URL: {url}");
            }
            return Err(err);
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from Azure OpenAI"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Azure OpenAI API key not set.")
        })?;

        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            messages: Self::convert_messages(request.messages),
            temperature,
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let url = self.build_url(_model);
        let response = self
            .http_client()
            .post(&url)
            .header("api-key", api_key)
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let mut err = super::api_error("Azure", response).await;
            if status == reqwest::StatusCode::NOT_FOUND {
                err = anyhow::anyhow!(
                    "{err}\nAttempted URL: {url}\n\nTip: For Azure OpenAI, the 'model' parameter must match your 'Deployment ID'. \
                    If you haven't created a deployment named '{_model}', please do so in Azure AI Studio or use a model name that matches an existing deployment."
                );
            } else {
                err = anyhow::anyhow!("{err}\nAttempted URL: {url}");
            }
            return Err(err);
        }

        let native_response: NativeChatResponse = response.json().await?;
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from Azure OpenAI"))?;
        Ok(Self::parse_native_response(message))
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Azure OpenAI API key not set.")
        })?;

        let native_tools: Option<Vec<NativeToolSpec>> = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .cloned()
                    .map(|v| serde_json::from_value(v).map_err(|e| anyhow::anyhow!("Invalid tool spec: {e}")))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };

        let native_request = NativeChatRequest {
            messages: Self::convert_messages(messages),
            temperature,
            tool_choice: native_tools.as_ref().map(|_| "auto".to_string()),
            tools: native_tools,
        };

        let response = self
            .http_client()
            .post(self.build_url(_model))
            .header("api-key", api_key)
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let url = self.build_url(_model);
            let mut err = super::api_error("Azure", response).await;
            if status == reqwest::StatusCode::NOT_FOUND {
                err = anyhow::anyhow!(
                    "{err}\nAttempted URL: {url}\n\nTip: For Azure OpenAI, the 'model' parameter must match your 'Deployment ID'. \
                    If you haven't created a deployment named '{_model}', please do so in Azure AI Studio or use a model name that matches an existing deployment."
                );
            } else {
                err = anyhow::anyhow!("{err}\nAttempted URL: {url}");
            }
            return Err(err);
        }

        let native_response: NativeChatResponse = response.json().await?;
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from Azure OpenAI"))?;
        Ok(Self::parse_native_response(message))
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        // No obvious healthcheck endpoint for Azure OpenAI that doesn't cost money or require a specific deployment.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_azure_provider() {
        let p = AzureOpenAiProvider::new("https://my-resource.openai.azure.com", Some("test-key"));
        assert_eq!(p.base_url, "https://my-resource.openai.azure.com");
        assert_eq!(p.api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn builds_correct_url() {
        let p = AzureOpenAiProvider::new("https://my-resource.openai.azure.com", Some("test-key"));
        let url = p.build_url("my-deployment");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/deployments/my-deployment/chat/completions?api-version=2024-02-15-preview"
        );
    }

    #[test]
    fn builds_correct_url_from_deployment_url() {
        let p = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com/openai/deployments/my-deployment",
            Some("test-key"),
        );
        let url = p.build_url("ignored");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/deployments/my-deployment?api-version=2024-02-15-preview"
        );
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AzureOpenAiProvider::new("https://example.com", None);
        let result = p.chat_with_system(None, "hello", "gpt-5.2", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }
}
