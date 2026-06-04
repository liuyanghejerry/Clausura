use crate::types::{
    ChatResponse, FinishReason, Message, ProviderError, Role, ToolCall, ToolDef, Usage,
    VendorConfig, VendorType,
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

/// Serialize messages in OpenAI-compatible format, including tool_call_id and tool_calls.
pub(crate) fn serialize_messages_openai(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            let mut obj = serde_json::json!({
                "role": serde_json::to_value(&m.role).unwrap_or_default(),
            });
            if m.role == Role::Assistant {
                if let Some(ref tc_vec) = m.tool_calls {
                    obj["content"] = serde_json::Value::Null;
                    let tool_calls_json: Vec<serde_json::Value> = tc_vec
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                                }
                            })
                        })
                        .collect();
                    obj["tool_calls"] = serde_json::json!(tool_calls_json);
                } else {
                    obj["content"] = serde_json::json!(m.content);
                }
            } else {
                obj["content"] = serde_json::json!(m.content);
            }
            if m.role == Role::Tool {
                if let Some(ref tcid) = m.tool_call_id {
                    obj["tool_call_id"] = serde_json::json!(tcid);
                }
            }
            obj
        })
        .collect()
}

/// Serialize tool definitions in OpenAI function-calling format.
pub(crate) fn serialize_tool_defs(tools: &[ToolDef]) -> serde_json::Value {
    serde_json::json!(tools
        .iter()
        .map(|t| serde_json::json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }
        }))
        .collect::<Vec<_>>())
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
    fn build_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDef]>,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": serialize_messages_openai(messages),
        });

        if let Some(tools) = tools {
            body["tools"] = serialize_tool_defs(tools);
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
    pub(crate) fn parse_response(
        response: serde_json::Value,
    ) -> Result<ChatResponse, ProviderError> {
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
            message: Message::new(role, content),
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
        // Use character-based heuristic (~3 chars/token for mixed text).
        // tiktoken (cl100k_base) is only accurate for OpenAI models;
        // other OpenAI-compatible providers (DeepSeek, Groq, Ollama) use
        // different tokenizers, making cl100k unreliable for budget enforcement.
        ((text.len() as f64 / 3.0).ceil() as u64).max(1)
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn vendor(&self) -> &str {
        "openai"
    }
}

// =========================================================================
// Anthropic-compatible provider
// =========================================================================

/// Anthropic Messages API provider.
pub struct AnthropicProvider {
    config: AnthropicProviderConfig,
    client: Client,
}

#[derive(Debug, Clone)]
pub struct AnthropicProviderConfig {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub max_retries: u32,
    pub timeout_secs: u64,
}

impl Default for AnthropicProviderConfig {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            api_key: String::new(),
            base_url: "https://api.anthropic.com".to_string(),
            max_retries: 3,
            timeout_secs: 60,
        }
    }
}

impl AnthropicProvider {
    pub fn new(config: AnthropicProviderConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(ProviderError::NetworkError)?;
        Ok(Self { config, client })
    }

    fn convert_tools_to_anthropic(tools: &[ToolDef]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    fn convert_messages_to_anthropic(messages: &[Message]) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        for msg in messages {
            match msg.role {
                Role::System => {
                    // Anthropic handles system as top-level field, not in messages
                    // We'll handle this in send_chat_request
                    continue;
                }
                Role::User => {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": msg.content,
                    }));
                }
                Role::Assistant => {
                    // Check if content looks like a tool call JSON
                    if let Ok(tool_calls) = serde_json::from_str::<Vec<ToolCall>>(&msg.content) {
                        let content_blocks: Vec<serde_json::Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.name,
                                    "input": tc.arguments,
                                })
                            })
                            .collect();
                        result.push(serde_json::json!({
                            "role": "assistant",
                            "content": content_blocks,
                        }));
                    } else {
                        result.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.content,
                        }));
                    }
                }
                Role::Tool => {
                    // Tool results: need to pair with the tool_use id
                    // Parse the tool result content to get the tool_use_id if present
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": "unknown",
                            "content": msg.content,
                        }],
                    }));
                }
            }
        }
        result
    }

    async fn send_chat_request(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDef]>,
    ) -> Result<ChatResponse, ProviderError> {
        let url = format!("{}/v1/messages", self.config.base_url);

        // Extract system message
        let system = messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let anthropic_messages = Self::convert_messages_to_anthropic(messages);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": 4096,
            "messages": anthropic_messages,
            "system": system,
        });

        if let Some(tools) = tools {
            body["tools"] = serde_json::json!(Self::convert_tools_to_anthropic(tools));
        }

        let mut last_error = None;
        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let base_delay = Duration::from_secs(1 << (attempt - 1));
                let jitter = Duration::from_millis(rand::random::<u64>() % 500);
                tokio::time::sleep(base_delay + jitter).await;
            }

            let response = self
                .client
                .post(&url)
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", "2023-06-01")
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
                        return Self::parse_response(&data);
                    } else if status.as_u16() == 429 {
                        last_error = Some(ProviderError::RateLimited("Rate limited".into()));
                        continue;
                    } else if status.is_server_error() {
                        last_error = Some(ProviderError::ServerError(format!("HTTP {}", status)));
                        continue;
                    } else if status.as_u16() == 401 || status.as_u16() == 403 {
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
                    if e.is_timeout() || e.is_connect() {
                        last_error = Some(ProviderError::NetworkError(e));
                        continue;
                    }
                    return Err(ProviderError::NetworkError(e));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ProviderError::ServerError("Max retries exceeded".into())))
    }

    fn parse_response(data: &serde_json::Value) -> Result<ChatResponse, ProviderError> {
        // Extract text content and tool calls from Anthropic response
        let content_blocks = data["content"]
            .as_array()
            .ok_or_else(|| ProviderError::BadRequest("Missing content array".into()))?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in content_blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str() {
                        text_parts.push(text.to_string());
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or("").to_string(),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        arguments: block["input"].clone(),
                    });
                }
                _ => {}
            }
        }

        let content = text_parts.join("\n");
        let finish_reason = match data["stop_reason"].as_str() {
            Some("end_turn") => FinishReason::Stop,
            Some("tool_use") => FinishReason::ToolCalls,
            Some("max_tokens") => FinishReason::Length,
            Some(other) => FinishReason::Other(other.to_string()),
            None => FinishReason::Stop,
        };

        let usage = data
            .get("usage")
            .map(|u| Usage {
                input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                total_tokens: 0,
            })
            .unwrap_or_default();

        let total_usage = Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.input_tokens + usage.output_tokens,
        };

        Ok(ChatResponse {
            message: Message::new(Role::Assistant, content),
            usage: total_usage,
            finish_reason,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
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
        // Anthropic models use ~3.5 chars per token for English
        (text.len() as f64 / 3.5).ceil().max(1.0) as u64
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn vendor(&self) -> &str {
        "anthropic"
    }
}

// =========================================================================
// Custom provider (user-defined enterprise LLM)
// =========================================================================

/// Custom provider for enterprise LLMs with configurable auth.
/// Uses OpenAI-compatible API format with custom base URL and auth header.
pub struct CustomProvider {
    config: CustomProviderConfig,
    client: Client,
}

#[derive(Debug, Clone)]
pub struct CustomProviderConfig {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub auth_header: String,
    pub max_retries: u32,
    pub timeout_secs: u64,
}

impl Default for CustomProviderConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            auth_header: "Authorization".to_string(),
            max_retries: 3,
            timeout_secs: 60,
        }
    }
}

impl CustomProvider {
    pub fn new(config: CustomProviderConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(ProviderError::NetworkError)?;
        Ok(Self { config, client })
    }
}

// Re-use the OpenAI-compatible request/response logic
// The CustomProvider delegates to the same OpenAI API format

#[async_trait]
impl Provider for CustomProvider {
    async fn chat(&self, messages: &[Message]) -> Result<ChatResponse, ProviderError> {
        self.chat_with_tools(messages, &[]).await
    }

    async fn chat_with_tools(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<ChatResponse, ProviderError> {
        let url = format!("{}/chat/completions", self.config.base_url);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": serialize_messages_openai(messages),
        });

        if !tools.is_empty() {
            body["tools"] = serialize_tool_defs(tools);
        }

        let mut last_error = None;
        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let base_delay = Duration::from_secs(1 << (attempt - 1));
                let jitter = Duration::from_millis(rand::random::<u64>() % 500);
                tokio::time::sleep(base_delay + jitter).await;
            }

            let mut request = self
                .client
                .post(&url)
                .header("Content-Type", "application/json");

            // Use custom auth header
            if self.config.auth_header.to_lowercase() == "authorization" {
                request = request.header(
                    &self.config.auth_header,
                    format!("Bearer {}", self.config.api_key),
                );
            } else {
                request = request.header(&self.config.auth_header, &self.config.api_key);
            }

            let response = request.json(&body).send().await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let text = resp.text().await.map_err(ProviderError::NetworkError)?;
                        let data: serde_json::Value =
                            serde_json::from_str(&text).map_err(ProviderError::JsonError)?;
                        return OpenAICompatibleProvider::parse_response(data);
                    } else if status.as_u16() == 429 {
                        last_error = Some(ProviderError::RateLimited("Rate limited".into()));
                        continue;
                    } else if status.is_server_error() {
                        last_error = Some(ProviderError::ServerError(format!("HTTP {}", status)));
                        continue;
                    } else if status.as_u16() == 401 || status.as_u16() == 403 {
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
                    if e.is_timeout() || e.is_connect() {
                        last_error = Some(ProviderError::NetworkError(e));
                        continue;
                    }
                    return Err(ProviderError::NetworkError(e));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ProviderError::ServerError("Max retries exceeded".into())))
    }

    fn count_tokens(&self, text: &str) -> u64 {
        (text.len() / 4).max(1) as u64
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn vendor(&self) -> &str {
        "custom"
    }
}

// =========================================================================
// Provider factory
// =========================================================================

/// Create the right provider based on vendor configuration.
pub fn create_provider(
    vendor: &VendorConfig,
    model: &str,
    api_key: &str,
    timeout_secs: u64,
) -> Result<Box<dyn Provider>, ProviderError> {
    match vendor.vendor_type {
        VendorType::OpenAiCompatible => {
            let provider = OpenAICompatibleProvider::new(OpenAIProviderConfig {
                model: model.to_string(),
                api_key: api_key.to_string(),
                base_url: vendor.effective_base_url().to_string(),
                timeout_secs,
                ..Default::default()
            })?;
            Ok(Box::new(provider))
        }
        VendorType::AnthropicCompatible => {
            let provider = AnthropicProvider::new(AnthropicProviderConfig {
                model: model.to_string(),
                api_key: api_key.to_string(),
                base_url: vendor.effective_base_url().to_string(),
                timeout_secs,
                ..Default::default()
            })?;
            Ok(Box::new(provider))
        }
        VendorType::Custom => {
            let provider = CustomProvider::new(CustomProviderConfig {
                model: model.to_string(),
                api_key: api_key.to_string(),
                base_url: vendor.effective_base_url().to_string(),
                auth_header: vendor.auth_header_name().to_string(),
                timeout_secs,
                ..Default::default()
            })?;
            Ok(Box::new(provider))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Mock provider for testing agent loop
    pub struct MockProvider {
        model: String,
        responses: Arc<Mutex<VecDeque<Result<ChatResponse, ProviderError>>>>,
        slow_responses: Arc<Mutex<VecDeque<Duration>>>,
    }

    impl MockProvider {
        pub fn new(model: &str) -> Self {
            Self {
                model: model.to_string(),
                responses: Arc::new(Mutex::new(VecDeque::new())),
                slow_responses: Arc::new(Mutex::new(VecDeque::new())),
            }
        }

        pub fn add_response(&mut self, response: ChatResponse) {
            self.responses.lock().unwrap().push_back(Ok(response));
        }

        pub fn add_slow_response(&mut self, delay: Duration) {
            self.slow_responses.lock().unwrap().push_back(delay);
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, messages: &[Message]) -> Result<ChatResponse, ProviderError> {
            self.chat_with_tools(messages, &[]).await
        }

        async fn chat_with_tools(
            &self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> Result<ChatResponse, ProviderError> {
            let delay = { self.slow_responses.lock().unwrap().pop_front() };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
                return Err(ProviderError::Timeout("Simulated timeout".into()));
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(ProviderError::ServerError(
                    "No more mock responses".into(),
                )))
        }

        fn count_tokens(&self, text: &str) -> u64 {
            (text.len() / 4).max(1) as u64
        }

        fn model(&self) -> &str {
            &self.model
        }

        fn vendor(&self) -> &str {
            "mock"
        }
    }

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
            .chat(&[Message::new(Role::User, "Hi")])
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
            .chat_with_tools(&[Message::new(Role::User, "Review diff")], &tools)
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
            .chat(&[Message::new(Role::User, "Hi")])
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

        let result = provider.chat(&[Message::new(Role::User, "Hi")]).await;

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

    #[tokio::test]
    async fn test_anthropic_simple_chat() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_123",
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Hello! How can I help?"}
                ],
                "model": "claude-sonnet-4-20250514",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            })))
            .mount(&mock_server)
            .await;

        let provider = AnthropicProvider::new(AnthropicProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-ant-test".into(),
            model: "claude-sonnet-4-20250514".into(),
            ..Default::default()
        })
        .unwrap();

        let response = provider
            .chat(&[Message::new(Role::User, "Hi")])
            .await
            .unwrap();

        assert_eq!(response.message.content, "Hello! How can I help?");
        assert_eq!(response.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn test_anthropic_tool_use() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_456",
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_01",
                        "name": "read_file",
                        "input": {"path": "src/main.rs"}
                    }
                ],
                "model": "claude-sonnet-4-20250514",
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 50, "output_tokens": 20}
            })))
            .mount(&mock_server)
            .await;

        let provider = AnthropicProvider::new(AnthropicProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-ant-test".into(),
            ..Default::default()
        })
        .unwrap();

        let tools = vec![ToolDef {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];

        let response = provider
            .chat_with_tools(&[Message::new(Role::User, "Read main.rs")], &tools)
            .await
            .unwrap();

        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        let calls = response.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }

    #[test]
    fn test_create_provider_openai() {
        let vendor = VendorConfig::openai();
        let provider = create_provider(&vendor, "gpt-4o", "sk-test", 60).unwrap();
        assert_eq!(provider.model(), "gpt-4o");
        assert_eq!(provider.vendor(), "openai");
    }

    #[test]
    fn test_create_provider_anthropic() {
        let vendor = VendorConfig::anthropic();
        let provider =
            create_provider(&vendor, "claude-sonnet-4-20250514", "sk-ant-test", 60).unwrap();
        assert_eq!(provider.model(), "claude-sonnet-4-20250514");
        assert_eq!(provider.vendor(), "anthropic");
    }

    #[test]
    fn test_create_provider_custom() {
        let vendor = VendorConfig {
            vendor_type: VendorType::Custom,
            base_url: Some("https://llm.internal/v1".into()),
            auth_header: Some("X-API-Key".into()),
            api_key_env: None,
        };
        let provider = create_provider(&vendor, "internal-model", "my-key", 60).unwrap();
        assert_eq!(provider.model(), "internal-model");
        assert_eq!(provider.vendor(), "custom");
    }

    // --- build_request_body tests ---

    fn make_provider() -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(OpenAIProviderConfig {
            base_url: "https://api.example.com/v1".into(),
            api_key: "sk-test".into(),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn test_build_body_includes_tool_call_id_for_tool_messages() {
        let provider = make_provider();
        let messages = vec![
            Message::new(Role::User, "run the tool"),
            Message::new(Role::Assistant, "calling tool"),
            Message::with_tool_call(Role::Tool, "tool result", "call_abc123".into()),
        ];
        let body = provider.build_request_body(&messages, None);
        let body_str = serde_json::to_string(&body).unwrap();
        assert!(
            body_str.contains("call_abc123"),
            "tool_call_id should be in request: {body_str}"
        );
        assert!(
            body_str.contains("tool_call_id"),
            "missing tool_call_id field"
        );
    }

    #[test]
    fn test_build_body_omits_tool_call_id_for_non_tool_messages() {
        let provider = make_provider();
        let messages = vec![
            Message::new(Role::System, "You are a helpful assistant."),
            Message::new(Role::User, "Hello"),
        ];
        let body = provider.build_request_body(&messages, None);
        let body_str = serde_json::to_string(&body).unwrap();
        assert!(
            !body_str.contains("tool_call_id"),
            "non-tool messages should not have tool_call_id"
        );
    }

    #[test]
    fn test_build_body_multi_turn_tool_call_round_trip() {
        let provider = make_provider();
        let messages = vec![
            Message::new(Role::System, "You have tools."),
            Message::new(Role::User, "Check the code"),
            Message::new(Role::Assistant, "Let me check"),
            Message::with_tool_call(Role::Tool, "diff output here", "call_1".into()),
            Message::new(Role::Assistant, "I see changes"),
            Message::with_tool_call(Role::Tool, "grep results", "call_2".into()),
            Message::new(Role::Assistant, "All done"),
        ];
        let body = provider.build_request_body(&messages, None);
        let body_str = serde_json::to_string(&body).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&body_str).unwrap();
        let msgs = parsed["messages"].as_array().unwrap();

        // Check tool messages have tool_call_id
        let tool_msg_1 = &msgs[3];
        assert_eq!(tool_msg_1["tool_call_id"], "call_1");
        assert_eq!(tool_msg_1["role"], "tool");

        let tool_msg_2 = &msgs[5];
        assert_eq!(tool_msg_2["tool_call_id"], "call_2");
        assert_eq!(tool_msg_2["role"], "tool");

        // Check non-tool messages do NOT have tool_call_id
        let user_msg = &msgs[1];
        assert_eq!(user_msg["role"], "user");
        assert!(user_msg.get("tool_call_id").is_none());
    }
}
