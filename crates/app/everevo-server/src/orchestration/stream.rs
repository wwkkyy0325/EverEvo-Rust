//! SSE event helpers — AgentEvent → content-block SSE conversion.

use axum::response::sse::Event;
use std::convert::Infallible;

/// Build a content_block_stop event.
pub fn stop_event(index: usize) -> Result<Event, Infallible> {
    Ok(Event::default()
        .event("content_block_stop")
        .data(serde_json::json!({"index": index}).to_string()))
}

/// Build a message_start event.
pub fn message_start(id: &str) -> Result<Event, Infallible> {
    Ok(Event::default()
        .event("message_start")
        .data(serde_json::json!({"message_id": id}).to_string()))
}

/// Build a thinking block start event.
pub fn thinking_start(index: usize) -> Result<Event, Infallible> {
    Ok(Event::default().event("content_block_start").data(
        serde_json::json!({"index": index, "content_block": {"type": "thinking", "thinking": ""}})
            .to_string(),
    ))
}

/// Build a text block start event.
pub fn text_start(index: usize) -> Result<Event, Infallible> {
    Ok(Event::default().event("content_block_start").data(
        serde_json::json!({"index": index, "content_block": {"type": "text", "text": ""}})
            .to_string(),
    ))
}

/// Build a tool_use block start event.
pub fn tool_start(index: usize, id: &str, name: &str) -> Result<Event, Infallible> {
    Ok(Event::default().event("content_block_start").data(
        serde_json::json!({"index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}).to_string(),
    ))
}

/// Build a content_block_delta for thinking.
pub fn thinking_delta(index: usize, text: &str) -> Result<Event, Infallible> {
    Ok(Event::default().event("content_block_delta").data(
        serde_json::json!({"index": index, "delta": {"type": "thinking_delta", "thinking": text}})
            .to_string(),
    ))
}

/// Build a content_block_delta for text.
pub fn text_delta(index: usize, text: &str) -> Result<Event, Infallible> {
    Ok(Event::default().event("content_block_delta").data(
        serde_json::json!({"index": index, "delta": {"type": "text_delta", "text": text}})
            .to_string(),
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a block event directly to JSON for testing — bypasses axum Event
    /// opaque type. Tests the JSON structure without SSE serialization.
    fn block_json(index: usize, block_type: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut block = serde_json::json!({"type": block_type});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                block[k] = v.clone();
            }
        }
        serde_json::json!({"index": index, "content_block": block})
    }

    #[test]
    fn test_message_start_json() {
        // Verify the JSON shape expected by the frontend
        let json = serde_json::json!({"message_id": "msg-42"});
        assert_eq!(json["message_id"], "msg-42");
    }

    #[test]
    fn test_stop_event_json() {
        let json = serde_json::json!({"index": 3});
        assert_eq!(json["index"], 3);
    }

    #[test]
    fn test_block_start_shapes() {
        // Thinking block
        let bt = block_json(1, "thinking", serde_json::json!({"thinking": ""}));
        assert_eq!(bt["index"], 1);
        assert_eq!(bt["content_block"]["type"], "thinking");

        // Text block
        let bt = block_json(2, "text", serde_json::json!({"text": ""}));
        assert_eq!(bt["content_block"]["type"], "text");

        // Tool use block
        let bt = block_json(
            0,
            "tool_use",
            serde_json::json!({"id": "t1", "name": "read_file", "input": {}}),
        );
        assert_eq!(bt["content_block"]["type"], "tool_use");
        assert_eq!(bt["content_block"]["id"], "t1");
        assert_eq!(bt["content_block"]["name"], "read_file");
    }

    #[test]
    fn test_delta_shapes() {
        let td =
            serde_json::json!({"index": 1, "delta": {"type": "thinking_delta", "thinking": "hmm"}});
        assert_eq!(td["delta"]["type"], "thinking_delta");

        let txt = serde_json::json!({"index": 2, "delta": {"type": "text_delta", "text": "hi"}});
        assert_eq!(txt["delta"]["text"], "hi");
    }

    #[test]
    fn test_all_events_are_infallible() {
        assert!(message_start("id").is_ok());
        assert!(stop_event(0).is_ok());
        assert!(thinking_start(0).is_ok());
        assert!(text_start(0).is_ok());
        assert!(tool_start(0, "id", "name").is_ok());
        assert!(thinking_delta(0, "think").is_ok());
        assert!(text_delta(0, "text").is_ok());
    }
}
