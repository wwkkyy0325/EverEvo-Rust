//! Provider-specific request-body serialization for the HTTP LLM client.
//!
//! `build_body` dispatches to the Anthropic or OpenAI body builder based on
//! the client's configured `api_format`. The per-provider builders are
//! private; only `build_body` is exposed to the coordinator module (`http`),
//! which calls it from both `chat` and `stream_chat`.

use super::HttpClient;
use everevo_core::llm::{LlmMessage, ToolSchema};
use serde_json;

impl HttpClient {
    pub(crate) fn build_body(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        stream: bool,
    ) -> serde_json::Value {
        if self.api_format == "anthropic" {
            self.build_anthropic_body(messages, tools, stream)
        } else {
            self.build_openai_body(messages, tools, stream)
        }
    }

    fn build_anthropic_body(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        stream: bool,
    ) -> serde_json::Value {
        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let is_tool_result = m.tool_call_id.is_some();
                let role_str = if is_tool_result {
                    "user".to_string()
                } else {
                    m.role.to_string()
                };

                let mut content_blocks: Vec<serde_json::Value> = Vec::new();

                if let Some(ref thinking) = m.thinking {
                    if !thinking.is_empty() {
                        content_blocks.push(serde_json::json!({
                            "type": "thinking",
                            "thinking": thinking,
                        }));
                    }
                }

                if !is_tool_result {
                    let has_thinking_or_tools = m.thinking.as_ref().is_some_and(|t| !t.is_empty())
                        || m.tool_calls.is_some();
                    if !m.content.is_empty() || !has_thinking_or_tools {
                        content_blocks.push(serde_json::json!({
                            "type": "text",
                            "text": m.content,
                        }));
                    }
                    // Multimodal: append image blocks (e.g. browser screenshots)
                    // for vision-capable models.
                    for img in &m.images {
                        content_blocks.push(serde_json::json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": img.mime_type, "data": img.data }
                        }));
                    }
                }
                if let Some(ref tcs) = m.tool_calls {
                    for tc in tcs {
                        content_blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                }
                if is_tool_result {
                    let Some(raw_id) = m.tool_call_id.as_ref() else {
                        tracing::warn!("Tool result message without tool_call_id, skipping");
                        return serde_json::json!({"role": "user", "content": ""});
                    };
                    if raw_id.contains('|') {
                        if let Ok(items) =
                            serde_json::from_str::<Vec<serde_json::Value>>(&m.content)
                        {
                            for item in &items {
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": item["i"].as_str().unwrap_or(""),
                                    "content": item["c"].as_str().unwrap_or(""),
                                }));
                            }
                        }
                    } else {
                        // Anthropic tool_result.content accepts an array of
                        // text/image blocks — used to carry screenshots back.
                        let content = if m.images.is_empty() {
                            serde_json::Value::String(m.content.clone())
                        } else {
                            let mut parts =
                                vec![serde_json::json!({ "type": "text", "text": m.content })];
                            for img in &m.images {
                                parts.push(serde_json::json!({
                                    "type": "image",
                                    "source": { "type": "base64", "media_type": img.mime_type, "data": img.data }
                                }));
                            }
                            serde_json::Value::Array(parts)
                        };
                        content_blocks.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": raw_id,
                            "content": content,
                        }));
                    }
                }

                serde_json::json!({ "role": role_str, "content": content_blocks })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 16384,
            "temperature": 0.0,
            "messages": msgs,
        });
        // Bound extended-thinking latency: DeepSeek v4-flash otherwise emits up
        // to max_tokens of thinking per request (60-100s round-trips → only ~3
        // rounds fit in the GAIA 300s wall-clock). EVEREVO_THINKING_BUDGET=N
        // caps thinking via budget_tokens (N>0); 0/unset = DeepSeek default.
        if let Ok(budget) = std::env::var("EVEREVO_THINKING_BUDGET") {
            if let Ok(n) = budget.trim().parse::<u32>() {
                if n > 0 {
                    body["thinking"] = serde_json::json!({"type": "enabled", "budget_tokens": n});
                }
            }
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools
                .iter()
                .map(|t| {
                    if let Some(native_type) = &t.native_type {
                        // Server-side tool: no input_schema — the API executes it.
                        serde_json::json!({
                            "name": t.name,
                            "type": native_type,
                            "description": t.description,
                        })
                    } else {
                        serde_json::json!({
                            "name": t.name, "description": t.description, "input_schema": t.parameters,
                        })
                    }
                })
                .collect::<Vec<_>>());
        }
        if stream {
            body["stream"] = serde_json::json!(true);
        }
        body
    }

    fn build_openai_body(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSchema],
        stream: bool,
    ) -> serde_json::Value {
        let mut msgs: Vec<serde_json::Value> = Vec::new();
        for m in messages {
            let role = m.role.to_string();

            if let Some(raw_id) = m.tool_call_id.as_ref() {
                if raw_id.contains('|') {
                    if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&m.content) {
                        for item in &items {
                            msgs.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": item["i"].as_str().unwrap_or(""),
                                "content": item["c"].as_str().unwrap_or(""),
                            }));
                        }
                    }
                }
                continue;
            }

            // OpenAI: tool-role messages can't carry images inline. Non-tool
            // messages with images use a content array (text + image_url).
            let is_tool_msg = m.tool_call_id.is_some();
            let content = if !is_tool_msg && !m.images.is_empty() {
                let mut parts = vec![serde_json::json!({ "type": "text", "text": m.content })];
                for img in &m.images {
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime_type, img.data) }
                    }));
                }
                serde_json::Value::Array(parts)
            } else {
                serde_json::Value::String(m.content.clone())
            };
            let mut msg = serde_json::json!({ "role": role, "content": content });
            if let Some(ref thinking) = m.thinking {
                if !thinking.is_empty() {
                    msg["reasoning_content"] = serde_json::json!(thinking);
                }
            }
            if let Some(ref tcs) = m.tool_calls {
                msg["tool_calls"] = serde_json::json!(tcs.iter().map(|tc| serde_json::json!({
                    "id": tc.id, "type": "function", "function": { "name": tc.name, "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default() },
                })).collect::<Vec<_>>());
                msg["content"] = serde_json::Value::Null;
            }
            if is_tool_msg {
                msg["role"] = serde_json::json!("tool");
                msg["tool_call_id"] = serde_json::json!(m.tool_call_id);
            }
            msgs.push(msg);

            // Tool-result images can't live in a tool-role message; emit a
            // follow-up user message so a vision model can see the screenshot.
            if is_tool_msg && !m.images.is_empty() {
                let mut img_parts = vec![serde_json::json!({
                    "type": "text", "text": "[screenshot from tool result]"
                })];
                for img in &m.images {
                    img_parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime_type, img.data) }
                    }));
                }
                msgs.push(serde_json::json!({ "role": "user", "content": img_parts }));
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "stream": stream,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools.iter().map(|t| serde_json::json!({
                "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
            })).collect::<Vec<_>>());
        }
        body
    }
}
