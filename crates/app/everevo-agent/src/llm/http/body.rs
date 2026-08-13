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
        let mut msgs: Vec<serde_json::Value> = messages
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
                        // DeepSeek's Anthropic endpoint rejects an empty text
                        // block ("all messages must have non-empty content",
                        // HTTP 400). A message with empty content, no thinking
                        // and no tools still needs a non-empty block — a single
                        // space is semantically inert.
                        let text = if m.content.is_empty() {
                            " ".to_string()
                        } else {
                            m.content.clone()
                        };
                        content_blocks.push(serde_json::json!({
                            "type": "text",
                            "text": text,
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
                        // Defensive: a tool call with null arguments would send
                        // `"input": null` — use an empty object instead.
                        let input = if tc.arguments.is_null() {
                            serde_json::json!({})
                        } else {
                            tc.arguments.clone()
                        };
                        content_blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": input,
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
                                // Same empty-content guard: a multi-tool turn
                                // where one tool (e.g. TodoWrite) returned
                                // nothing would otherwise send an empty
                                // tool_result block and trigger DeepSeek's
                                // HTTP 400 "non-empty content" rejection.
                                let c = item["c"].as_str().unwrap_or("");
                                let c = if c.is_empty() { " " } else { c };
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": item["i"].as_str().unwrap_or(""),
                                    "content": c,
                                }));
                            }
                        } else {
                            // Payload is not JSON (e.g. an old multi-tool result
                            // that was masked to a plain header) — emit one
                            // tool_result per id from tool_call_id so every
                            // tool_use keeps its tool_result and DeepSeek's
                            // strict alternation holds. Content is the raw
                            // (masked) string, guarded non-empty.
                            let c = if m.content.is_empty() {
                                " ".to_string()
                            } else {
                                m.content.clone()
                            };
                            for id in raw_id.split('|') {
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": id,
                                    "content": c,
                                }));
                            }
                        }
                    } else {
                        // Anthropic tool_result.content accepts an array of
                        // text/image blocks — used to carry screenshots back.
                        let content = if m.images.is_empty() {
                            // Same DeepSeek guard as the text block above: a
                            // tool (e.g. a shell command with no stdout) that
                            // returns empty content must not send an empty
                            // tool_result, or the API rejects the message.
                            let c = if m.content.is_empty() {
                                " ".to_string()
                            } else {
                                m.content.clone()
                            };
                            serde_json::Value::String(c)
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

        // DeepSeek's Anthropic endpoint rejects any message whose content is
        // empty ("all messages must have non-empty content", HTTP 400). Empty
        // content reaches this layer from many producers — an empty tool result
        // (shell with no stdout, an empty web_fetch page), an empty assistant
        // turn, an empty thinking-only response — at a message index that varies
        // with the conversation (observed messages.11 / .14 / .23 in the
        // 2026-08-12 full GAIA run). The per-block guards above cover the known
        // producers; this pass is a belt-and-suspenders sweep that guarantees no
        // text block or tool_result payload stays empty, whatever produced it.
        for (msg_idx, msg) in msgs.iter_mut().enumerate() {
            let mut fixed = false;
            match msg.get_mut("content") {
                Some(serde_json::Value::Array(blocks)) => {
                    for block in blocks.iter_mut() {
                        match block["type"].as_str() {
                            Some("text") => {
                                if block["text"].as_str().unwrap_or("").is_empty() {
                                    block["text"] = serde_json::json!(" ");
                                    fixed = true;
                                }
                            }
                            Some("tool_result") => match block.get_mut("content") {
                                Some(serde_json::Value::String(s)) => {
                                    if s.is_empty() {
                                        *s = " ".to_string();
                                        fixed = true;
                                    }
                                }
                                Some(serde_json::Value::Array(inner)) => {
                                    for ib in inner.iter_mut() {
                                        if ib["type"] == "text"
                                            && ib["text"].as_str().unwrap_or("").is_empty()
                                        {
                                            ib["text"] = serde_json::json!(" ");
                                            fixed = true;
                                        }
                                    }
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                    if blocks.is_empty() {
                        blocks.push(serde_json::json!({"type": "text", "text": " "}));
                        fixed = true;
                    }
                }
                Some(serde_json::Value::String(s)) if s.is_empty() => {
                    *s = " ".to_string();
                    fixed = true;
                }
                _ => {}
            }
            if fixed {
                tracing::warn!(
                    msg_idx,
                    role = msg["role"].as_str().unwrap_or("?"),
                    "Sanitized empty content for DeepSeek (HTTP 400 guard)"
                );
            }
        }

        // Output budget per request (thinking + content). Tunable via
        // EVEREVO_MAX_OUTPUT_TOKENS so a long-budget GAIA run can give the
        // model more reasoning room (default 16384).
        let max_output: u32 = std::env::var("EVEREVO_MAX_OUTPUT_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(16_384);
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_output,
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

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::llm::{LlmMessage, LlmRole};

    fn client() -> HttpClient {
        HttpClient::new("anthropic", "test-key", "http://localhost:1", "test-model")
    }

    // Regression: DeepSeek's Anthropic endpoint returns HTTP 400
    // ("all messages must have non-empty content") when a message carries an
    // empty text block or an empty tool_result. The 2026-08-12 full GAIA run
    // hit this on ~40% of questions (messages.11) — an empty shell result or
    // empty assistant turn was serialized as `"text": ""` / `"content": ""`.
    #[test]
    fn anthropic_body_never_sends_empty_text_block() {
        let empty_assistant = LlmMessage {
            role: LlmRole::Assistant,
            content: String::new(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        };
        let body = client().build_body(&[empty_assistant], &[], false);
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        let text = blocks
            .iter()
            .find(|b| b["type"] == "text")
            .and_then(|b| b["text"].as_str())
            .unwrap_or("");
        assert!(
            !text.is_empty(),
            "empty text block must be guarded with a non-empty placeholder"
        );
    }

    #[test]
    fn anthropic_body_guards_empty_tool_result() {
        let empty_tool = LlmMessage {
            role: LlmRole::User,
            content: String::new(),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            images: Vec::new(),
        };
        let body = client().build_body(&[empty_tool], &[], false);
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        let content = blocks[0]["content"].as_str().unwrap_or("");
        assert!(
            !content.is_empty(),
            "empty tool_result content must be guarded with a placeholder"
        );
    }

    #[test]
    fn anthropic_body_guards_null_tool_use_input() {
        let tc = everevo_core::types::ToolCall {
            id: "call_1".into(),
            name: "todo_write".into(),
            arguments: serde_json::Value::Null,
        };
        let msg = LlmMessage {
            role: LlmRole::Assistant,
            content: String::new(),
            thinking: None,
            tool_calls: Some(vec![tc]),
            tool_call_id: None,
            images: Vec::new(),
        };
        let body = client().build_body(&[msg], &[], false);
        let input = &body["messages"][0]["content"][0]["input"];
        assert!(
            !input.is_null(),
            "null tool_use input must be replaced with {{}}"
        );
    }

    #[test]
    fn anthropic_body_falls_back_for_nonjson_multi_tool_payload() {
        // An old multi-tool result masked to a plain header keeps tool_call_id
        // ("id1|id2") but its content is no longer JSON. The builder must emit
        // one tool_result per id so the preceding tool_use ids aren't orphaned
        // (DeepSeek HTTP 400 "tool_use ids were found without tool_result").
        let masked = LlmMessage {
            role: LlmRole::User,
            content: "[tool result from \"shell\" masked; 1200 bytes elided...]".into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some("call_00_a|call_01_b".into()),
            images: Vec::new(),
        };
        let body = client().build_body(&[masked], &[], false);
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2, "must emit one tool_result per id");
        assert_eq!(blocks[0]["tool_use_id"], "call_00_a");
        assert_eq!(blocks[1]["tool_use_id"], "call_01_b");
        assert!(!blocks[0]["content"].as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn anthropic_body_guards_empty_multi_tool_result() {
        // A multi-tool turn (tool_call_id joined with '|') where one tool
        // returned nothing serializes each tool_result separately — the empty
        // one must still carry a placeholder.
        let multi = LlmMessage {
            role: LlmRole::User,
            content: r#"[{"i":"call_1","c":""},{"i":"call_2","c":"result"}]"#.into(),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some("call_1|call_2".into()),
            images: Vec::new(),
        };
        let body = client().build_body(&[multi], &[], false);
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        let first = blocks[0]["content"].as_str().unwrap_or("");
        assert!(
            !first.is_empty(),
            "empty multi-tool result must be guarded with a placeholder"
        );
        // The non-empty sibling survives untouched.
        assert_eq!(blocks[1]["content"], "result");
    }
}
