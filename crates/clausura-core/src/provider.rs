use crate::types::{
    ChatResponse, FinishReason, Message, ProviderError, Role, ToolCall, ToolDef, Usage,
};
use async_trait::async_trait;
use reqwest::Client;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// All LLM providers implement this trait.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Send a chat completion request (non-streaming).
    async fn chat(&self, messages: &[Message]) -> Result<ChatResponse, ProviderError>;

    /// Send a chat completion with tool definitions.
    async fn chat_with_tools(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<ChatResponse, ProviderError>;

    /// Count tokens in text using the provider-specific tokenizer.
    fn count_tokens(&self, text: &str) -> u64;

    /// Get the model name.
    fn model(&self) -> &str;

    /// Get the vendor name (e.g. "openai", "anthropic").
    fn vendor(&self) -> &str;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible provider
// ---------------------------------------------------------------------------

/// Configuration for [`OpenAICompatibleProvider`].
#[derive(Debug, Clone)]
pub struct OpenAIProviderConfig {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub max_retries: u32,
    pub timeout_secs: u64,
}

impl Default for OpenAIProviderConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            max_retries: 3,
            timeout_secs: 60,
        }
    }
}

/// Provider that calls any OpenAI-compatible chat completions API.
///
/// Works with OpenAI, DeepSeek, Groq, Ollama (via openai proxy), etc.
pub struct OpenAICompatibleProvider {
    config: OpenAIProviderConfig,
    client: Client,
}

impl OpenAICompatibleProvider {
    pub fn new(config: OpenAIProviderConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(ProviderError::NetworkError)?;
        Ok(Self { config, client })
    }

    /// Internal helper: build the JSON request body.
    fn build_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDef]>,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages.iter().map(|m| serde_json::json!({
                "role": serde_json::to_value(&m.role).unwrap_or_default(),
                "content": m.content,
            })).collect::<Vec<_>>(),
        });

        if let Some(tools) = tools {
            body["tools"] = serde_json::json!(tools
                .iter()
                .map(|t| serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                }))
                .collect::<Vec<_>>());
        }

        body
    }

    /// Send a chat request with optional tools and automatic retry logic.
    async fn send_chat_request(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDef]>,
    ) -> Result<ChatResponse, ProviderError> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let body = self.build_request_body(messages, tools);

        let mut last_error = None;
        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                // Exponential backoff with jitter: 1s, 2s, 4s, ...
                let base_delay = Duration::from_secs(1 << (attempt - 1));
                let jitter_ms: u64 = rand::random::<u64>() % 500;
                tokio::time::sleep(base_delay + Duration::from_millis(jitter_ms)).await;
            }

            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let text = resp.text().await.map_err(ProviderError::NetworkError)?;
                        let data: serde_json::Value =
                            serde_json::from_str(&text).map_err(ProviderError::JsonError)?;
                        return Self::parse_response(data);
                    } else if status.as_u16() == 429 {
                        last_error = Some(ProviderError::RateLimited("Rate limited".into()));
                        continue;
                    } else if status.is_server_error() {
                        last_error = Some(ProviderError::ServerError(format!("HTTP {}", status)));
                        continue;
                    } else if status.as_u16() == 401 {
                        return Err(ProviderError::AuthError("Invalid API key".into()));
                    } else {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::BadRequest(format!(
                            "HTTP {}: {}",
                            status, text
                        )));
                    }
                }
                Err(e) => {
                    if e.is_timeout() {
                        last_error = Some(ProviderError::Timeout("Request timed out".into()));
                        continue;
                    } else if e.is_connect() {
                        last_error = Some(ProviderError::NetworkError(e));
                        continue;
                    } else {
                        return Err(ProviderError::NetworkError(e));
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ProviderError::ServerError("Max retries exceeded".into())))
    }

    /// Parse the OpenAI-style JSON response into our `ChatResponse`.
    fn parse_response(response: serde_json::Value) -> Result<ChatResponse, ProviderError> {
        let choice = response["choices"][0]
            .as_object()
            .ok_or_else(|| ProviderError::BadRequest("Missing choices[0]".into()))?;

        let message = choice["message"]
            .as_object()
            .ok_or_else(|| ProviderError::BadRequest("Missing message".into()))?;

        let content = message["content"].as_str().unwrap_or_default().to_string();
        let role_str = message["role"].as_str().unwrap_or("assistant");

        let role = match role_str {
            "assistant" => Role::Assistant,
            "user" => Role::User,
            "system" => Role::System,
            "tool" => Role::Tool,
            _ => Role::Assistant,
        };

        let finish_reason_str = choice["finish_reason"].as_str().unwrap_or("stop");
        let finish_reason = match finish_reason_str {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_string()),
        };

        let tool_calls = message.get("tool_calls").map(|tc| {
            tc.as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| {
                            Some(ToolCall {
                                id: t["id"].as_str()?.to_string(),
                                name: t["function"]["name"].as_str()?.to_string(),
                                arguments: t["function"]["arguments"]
                                    .as_str()
                                    .and_then(|s| serde_json::from_str(s).ok())
                                    .unwrap_or(serde_json::Value::Null),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });

        let usage = response
            .get("usage")
            .map(|u| Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
            })
            .unwrap_or_default();

        Ok(ChatResponse {
            message: Message { role, content },
            usage,
            finish_reason,
            tool_calls,
        })
    }
}

#[async_trait]
impl Provider for OpenAICompatibleProvider {
    async fn chat(&self, messages: &[Message]) -> Result<ChatResponse, ProviderError> {
        self.send_chat_request(messages, None).await
    }

    async fn chat_with_tools(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<ChatResponse, ProviderError> {
        self.send_chat_request(messages, Some(tools)).await
    }

    fn count_tokens(&self, text: &str) -> u64 {
        // Use tiktoken for OpenAI models, fallback to approximation
        match tiktoken_rs::cl100k_base() {
            Ok(bpe) => bpe.encode_with_special_tokens(text).len() as u64,
            Err(_) => {
                // Fallback: ~4 chars per token for English text
                (text.len() / 4).max(1) as u64
            }
        }
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn vendor(&self) -> &str {
        "openai"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_simple_chat() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-123",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I help?"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = OpenAICompatibleProvider::new(OpenAIProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-test".into(),
            model: "gpt-4o".into(),
            ..Default::default()
        })
        .unwrap();

        let response = provider
            .chat(&[Message {
                role: Role::User,
                content: "Hi".into(),
            }])
            .await
            .unwrap();

        assert_eq!(response.message.content, "Hello! How can I help?");
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(response.usage.input_tokens, 10);
    }

    #[tokio::test]
    async fn test_tool_calling() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-456",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_abc123",
                            "type": "function",
                            "function": {
                                "name": "git_diff",
                                "arguments": "{\"base\": \"HEAD~1\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 50,
                    "completion_tokens": 10,
                    "total_tokens": 60
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = OpenAICompatibleProvider::new(OpenAIProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-test".into(),
            ..Default::default()
        })
        .unwrap();

        let tools = vec![ToolDef {
            name: "git_diff".into(),
            description: "Get git diff".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "base": {"type": "string"}
                }
            }),
        }];

        let response = provider
            .chat_with_tools(
                &[Message {
                    role: Role::User,
                    content: "Review diff".into(),
                }],
                &tools,
            )
            .await
            .unwrap();

        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        let calls = response.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "git_diff");
    }

    #[tokio::test]
    async fn test_retry_on_429() {
        let mock_server = MockServer::start().await;

        // First call: 429, subsequent calls: 200
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(2)
            .mount(&mock_server)
            .await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "OK after retry"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })))
            .mount(&mock_server)
            .await;

        let provider = OpenAICompatibleProvider::new(OpenAIProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-test".into(),
            max_retries: 3,
            ..Default::default()
        })
        .unwrap();

        let response = provider
            .chat(&[Message {
                role: Role::User,
                content: "Hi".into(),
            }])
            .await
            .unwrap();

        assert_eq!(response.message.content, "OK after retry");
    }

    #[tokio::test]
    async fn test_no_retry_on_401() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&mock_server)
            .await;

        let provider = OpenAICompatibleProvider::new(OpenAIProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-wrong".into(),
            ..Default::default()
        })
        .unwrap();

        let result = provider
            .chat(&[Message {
                role: Role::User,
                content: "Hi".into(),
            }])
            .await;

        assert!(matches!(result, Err(ProviderError::AuthError(_))));
    }

    #[test]
    fn test_count_tokens() {
        let provider = OpenAICompatibleProvider::new(OpenAIProviderConfig {
            api_key: "sk-test".into(),
            ..Default::default()
        })
        .unwrap();

        let count = provider.count_tokens("Hello, world!");
        assert!(count > 0, "Token count should be > 0");
    }
}
