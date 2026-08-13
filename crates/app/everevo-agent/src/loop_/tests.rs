#[cfg(test)]
mod tests {
    use crate::llm::MockLlmProvider;
    use crate::loop_::config::RunConfig;
    use crate::loop_::convergence::{
        budget_line, convergence_stage, forced_final_prompt, Convergence,
    };
    use crate::loop_::driver::run_loop;
    use crate::loop_::proactivity::{hash_args, EscalationLevel, ProactivityState};
    use crate::loop_::trim;
    use crate::loop_::{AgentEvent, AgentLoop};
    use everevo_core::llm::{LlmMessage, LlmProvider, LlmRole, StreamEvent, ToolSchema};
    use everevo_core::tool::{Tool, ToolOutput, ToolRegistry};
    use everevo_core::EverEvoError;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
        }
        fn risk_level(&self) -> everevo_core::types::RiskLevel {
            everevo_core::types::RiskLevel::Low
        }
        async fn execute(
            &self,
            params: serde_json::Value,
            _cancel: Option<&CancellationToken>,
        ) -> Result<ToolOutput, EverEvoError> {
            let text = params["text"].as_str().unwrap_or("no input");
            Ok(ToolOutput {
                content: format!("echo: {text}"),
                is_error: false,
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn test_agent_direct_answer_no_tools() {
        let mock = MockLlmProvider::new().with_text("Hello, how can I help?");
        let resp = mock.chat(&[LlmMessage::user("hi")], &[]).await.unwrap();
        assert_eq!(resp.content.unwrap(), "Hello, how can I help?");
    }

    #[tokio::test]
    async fn test_agent_with_tool_call_response() {
        let mock = MockLlmProvider::new()
            .with_tool_call("echo", serde_json::json!({"text": "hello"}))
            .with_text("The tool returned: echo: hello");

        let messages = vec![LlmMessage::user("echo hello")];
        let resp = mock.chat(&messages, &[]).await.unwrap();
        assert_eq!(
            resp.tool_calls.len(),
            1,
            "First response should be the tool call"
        );
        assert_eq!(resp.tool_calls[0].name, "echo");
        assert_eq!(
            resp.tool_calls[0].arguments,
            serde_json::json!({"text": "hello"})
        );
    }

    #[test]
    fn test_tool_registry_with_echo() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("echo").is_some());
    }

    #[tokio::test]
    async fn test_echo_tool_execute() {
        let tool = EchoTool;
        let output = tool
            .execute(serde_json::json!({"text": "world"}), None)
            .await
            .unwrap();
        assert_eq!(output.content, "echo: world");
        assert!(!output.is_error);
    }

    #[tokio::test]
    async fn test_agent_loop_creation() {
        let agent = AgentLoop::new();
        assert_eq!(agent.max_turns, 0);
        let limited = agent.with_max_turns(5);
        assert_eq!(limited.max_turns, 5);
    }

    // ── Convergence nudge thresholds (benchmark mode) ─────────────────────

    fn is_commit(s: Convergence) -> bool {
        matches!(s, Convergence::Commit)
    }
    fn is_converge(s: Convergence) -> bool {
        matches!(s, Convergence::Converge)
    }

    #[test]
    fn test_convergence_turn_thresholds() {
        // max_turns=10: turn 7 → 70% → Converge; turn 9 → 90% → Commit.
        assert!(is_converge(convergence_stage(7, 10, 1.0)));
        assert!(is_commit(convergence_stage(9, 10, 1.0)));
        // Early turns stay None regardless of wall-clock.
        assert!(matches!(convergence_stage(3, 10, 1.0), Convergence::None));
        // Boundary: exactly 70% is Converge, 85% is Commit.
        assert!(is_converge(convergence_stage(7, 10, 1.0)));
        assert!(is_commit(convergence_stage(9, 10, 1.0)));
    }

    #[test]
    fn test_convergence_wall_clock_thresholds() {
        // Wall-clock alone drives convergence when turns are unbounded.
        assert!(matches!(convergence_stage(1, 0, 0.5), Convergence::None));
        assert!(is_converge(convergence_stage(1, 0, 0.30)));
        assert!(is_commit(convergence_stage(1, 0, 0.15)));
        // Wall-clock can force Commit even when turn budget is fresh.
        assert!(is_commit(convergence_stage(1, 10, 0.10)));
    }

    #[test]
    fn test_budget_line_format() {
        assert_eq!(
            budget_line(Some(3), Some(90)),
            "[Budget: 3 turns left, ~90s wall-clock left]"
        );
        assert_eq!(
            budget_line(None, Some(90)),
            "[Budget: unbounded turns left, ~90s wall-clock left]"
        );
        assert_eq!(budget_line(Some(1), None), "[Budget: 1 turns left]");
    }

    #[test]
    fn test_forced_final_prompt_contains_marker() {
        let p = forced_final_prompt();
        assert!(p.contains("Final answer: <value>"));
        assert!(p.contains("Do NOT call any tools"));
    }

    // ── Thinking-only turn must not commit an empty answer ────────────────
    // Regression test for the GAIA run-4 empty-prediction failures (Q3/Q37/Q46):
    // when the model streams reasoning but no text and no tool call, the loop
    // used to commit `final_text=""`, which the harness scores as a FAIL. It
    // must instead push the reasoning back and run one no-tool convergence call
    // so a value actually gets committed.

    #[tokio::test]
    async fn test_thinking_only_forces_terminal_convergence() {
        use crate::llm::MockLlmProvider;

        // First stream: reasoning only, no text, no tool call.
        let mock = MockLlmProvider::new()
            .with_stream(vec![
                StreamEvent::Thinking("Let me enumerate the box placements...".into()),
                StreamEvent::Done {
                    input_tokens: 100,
                    output_tokens: 50,
                    stop_reason: Some("end_turn".into()),
                },
            ])
            // Second call = the forced convergence chat() — commit a value.
            .with_text("Final answer: 16000");

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let tool_schemas: Vec<ToolSchema> = reg
            .as_tool_schemas()
            .into_iter()
            .map(|s| ToolSchema {
                name: s["function"]["name"].as_str().unwrap_or("").into(),
                description: s["function"]["description"].as_str().unwrap_or("").into(),
                parameters: s["function"]["parameters"].clone(),
                native_type: None,
            })
            .collect();
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        let mut messages = vec![LlmMessage::user("solve it")];

        let config = RunConfig {
            max_turns: 3,
            pending_subagents: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            ..RunConfig::new()
        };
        run_loop(&mock, &reg, &tool_schemas, &mut messages, config, &tx)
            .await
            .expect("run_loop should not error");

        // Drop the sender so the receiver below sees channel close (run_loop
        // only borrows tx; without this, recv() would wait forever).
        drop(tx);

        let mut final_text = String::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::Done { final_text: ft } = ev {
                final_text = ft;
            }
        }
        assert!(
            !final_text.is_empty(),
            "thinking-only turn must not commit an empty final answer"
        );
        assert_eq!(final_text, "Final answer: 16000");

        // The convergence call must have happened: 2 LLM calls (1 stream + 1 chat).
        assert_eq!(mock.call_count(), 2);
    }

    // ── Id-less argument deltas (llama-server OpenAI stream) must accumulate ──
    // Regression test for the 2026-08-12 GAIA local-2B smoke: llama-server
    // streams a tool call's `arguments` as a first chunk WITH `id` (a lone `{`),
    // then continuation chunks WITHOUT `id` (`"query":"..."`). The loop's strict
    // `pending_id == &id` check dropped every continuation chunk, leaving just
    // `{` which never parsed as JSON → every tool failed with "Missing '<param>'".

    #[tokio::test]
    async fn test_stream_idless_arg_deltas_accumulate() {
        use crate::llm::MockLlmProvider;

        // Mimics llama-server's OpenAI-compat tool-call emission: the id appears
        // only on the first chunk; subsequent chunks carry bare argument deltas.
        let mock = MockLlmProvider::new()
            .with_stream(vec![
                StreamEvent::ToolCallStart {
                    id: "call_1".into(),
                    name: "echo".into(),
                },
                StreamEvent::ToolCallArg {
                    id: "call_1".into(),
                    arg_delta: "{".into(),
                },
                StreamEvent::ToolCallArg {
                    id: String::new(),
                    arg_delta: "\"text\":\"".into(),
                },
                StreamEvent::ToolCallArg {
                    id: String::new(),
                    arg_delta: "hello".into(),
                },
                StreamEvent::ToolCallArg {
                    id: String::new(),
                    arg_delta: "\"}".into(),
                },
                StreamEvent::Done {
                    input_tokens: 10,
                    output_tokens: 10,
                    stop_reason: Some("tool_calls".into()),
                },
            ])
            // Second call: the tool result round-trips and the model answers.
            .with_text("The tool returned: echo: hello");

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let tool_schemas: Vec<ToolSchema> = reg
            .as_tool_schemas()
            .into_iter()
            .map(|s| ToolSchema {
                name: s["function"]["name"].as_str().unwrap_or("").into(),
                description: s["function"]["description"].as_str().unwrap_or("").into(),
                parameters: s["function"]["parameters"].clone(),
                native_type: None,
            })
            .collect();
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        let mut messages = vec![LlmMessage::user("echo hello")];

        let config = RunConfig {
            max_turns: 3,
            pending_subagents: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            ..RunConfig::new()
        };
        run_loop(&mock, &reg, &tool_schemas, &mut messages, config, &tx)
            .await
            .expect("run_loop should not error");

        drop(tx);

        // The driver emits AgentEvent::ToolCallStart with the parsed arguments.
        // The old bug dropped every delta after the first `{`, so the accumulated
        // string was just `{` — which fails to parse — and arguments came out as
        // Value::Null. With the fix the full JSON object must survive.
        let mut saw_valid_args = false;
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::ToolCallStart {
                name, arguments, ..
            } = ev
            {
                assert_eq!(name, "echo");
                assert_eq!(
                    arguments,
                    serde_json::json!({"text": "hello"}),
                    "id-less continuation deltas must accumulate into valid JSON args"
                );
                saw_valid_args = true;
            }
        }
        assert!(saw_valid_args, "expected the echo tool call to be emitted");
    }

    // ── Wall-clock hard-stop (benchmark mode) must break before the harness kill ──
    // Regression test for the 2026-08-12 GAIA local-2B smoke: Q1/Q2 both hit the
    // external 300s kill with EMPTY predictions because the loop only force-commits
    // at max_turns, and the slow 2B model (15-25s/turn) never reaches turn 20 —
    // the convergence nudges are just text the model ignores. With a near-expired
    // wall-clock deadline the loop must break early and run the forced terminal
    // commit so a value gets emitted instead of a silent empty pred.

    #[tokio::test]
    async fn test_wallclock_hardstop_forces_terminal_commit() {
        use crate::llm::MockLlmProvider;
        use std::time::{Duration, Instant};

        let tool_stream = |id: &str, text: &str| {
            vec![
                StreamEvent::ToolCallStart {
                    id: id.into(),
                    name: "echo".into(),
                },
                StreamEvent::ToolCallArg {
                    id: id.into(),
                    arg_delta: "{".into(),
                },
                StreamEvent::ToolCallArg {
                    id: String::new(),
                    arg_delta: format!("\"text\":\"{text}\""),
                },
                StreamEvent::ToolCallArg {
                    id: String::new(),
                    arg_delta: "\"}".into(),
                },
                StreamEvent::Done {
                    input_tokens: 10,
                    output_tokens: 10,
                    stop_reason: Some("tool_calls".into()),
                },
            ]
        };
        let mock = MockLlmProvider::new()
            .with_stream(tool_stream("call_1", "first"))
            .with_stream(tool_stream("call_2", "second"))
            .with_text("Final answer: 42");

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let tool_schemas: Vec<ToolSchema> = reg
            .as_tool_schemas()
            .into_iter()
            .map(|s| ToolSchema {
                name: s["function"]["name"].as_str().unwrap_or("").into(),
                description: s["function"]["description"].as_str().unwrap_or("").into(),
                parameters: s["function"]["parameters"].clone(),
                native_type: None,
            })
            .collect();
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        let mut messages = vec![LlmMessage::user("echo hello")];

        // Deadline already in the past: the loop must stop after turn 1 and run
        // the forced no-tool commit instead of spinning more tool turns.
        let wall_deadline = Some(Instant::now() + Duration::from_millis(1));
        let config = RunConfig {
            max_turns: 20, // far above the reached turn count — the break must trigger first
            wall_clock_deadline: wall_deadline,
            pending_subagents: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            ..RunConfig::new()
        };
        run_loop(&mock, &reg, &tool_schemas, &mut messages, config, &tx)
            .await
            .expect("run_loop should not error");

        drop(tx);

        // Without the hard-stop break the loop would consume the second tool
        // stream too (3 LLM calls total: stream, stream, plain-text fallback).
        // With the break only 2 calls happen: stream then forced-commit chat.
        assert_eq!(
            mock.call_count(),
            2,
            "wall-clock deadline must break the loop after one turn and \
             run exactly one forced-commit chat call"
        );
        let mut done_final = None;
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::Done { final_text } = ev {
                done_final = Some(final_text);
            }
        }
        assert_eq!(
            done_final.as_deref(),
            Some("Final answer: 42"),
            "forced terminal commit must emit the model's extracted value"
        );
    }

    #[test]
    fn test_truncate_output_short() {
        let result = trim::truncate_output("hello", 4000);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_output_long() {
        let long = "A".repeat(5000);
        let result = trim::truncate_output(&long, 1000);
        assert!(result.len() <= 1200);
        assert!(result.contains("[truncated: 5000 total chars"));
        assert!(result.starts_with('A'));
        assert!(result.ends_with('A'));
    }

    // ── ProactivityState tests ──────────────────────────────────────────

    #[test]
    fn test_proactivity_starts_normal() {
        let ps = ProactivityState::new();
        assert_eq!(ps.level, EscalationLevel::Normal);
        assert!(ps.intervention_message().is_none());
    }

    #[test]
    fn test_escalation_detects_fixation() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "cargo build"}));

        // Same tool, same args, error — once (no escalation yet)
        ps.update("shell", true, args_h, None);
        assert_eq!(ps.level, EscalationLevel::Normal);

        // Same again — now L1 Hint
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::Hint);
        assert!(ps.intervention_message().is_some());

        // Same again — L2 ResearchRequired
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::ResearchRequired);

        // Same again — L3 ForcedDivergence
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::ForcedDivergence);
    }

    #[test]
    fn test_escalation_resets_on_new_approach() {
        let mut ps = ProactivityState::new();
        let shell_args = hash_args(&serde_json::json!({"cmd": "cargo build"}));

        // Get to L2
        ps.update("shell", true, shell_args, None);
        ps.update("shell", true, shell_args, Some(("shell", shell_args)));
        ps.update("shell", true, shell_args, Some(("shell", shell_args)));
        assert_eq!(ps.level, EscalationLevel::ResearchRequired);

        // Switch to a different tool (web_search) — should not escalate further
        let ws_args = hash_args(&serde_json::json!({"query": "cargo build error"}));
        ps.update("web_search", false, ws_args, Some(("shell", shell_args)));
        // Web_search succeeded, approach changed → full reset
        assert_eq!(ps.level, EscalationLevel::Normal);
        assert!(ps.intervention_message().is_none());
    }

    #[test]
    fn test_escalation_ignores_different_errors() {
        let mut ps = ProactivityState::new();
        let args1 = hash_args(&serde_json::json!({"cmd": "cargo build"}));
        let args2 = hash_args(&serde_json::json!({"file": "src/main.rs"}));

        // First error with shell
        ps.update("shell", true, args1, None);
        assert_eq!(ps.level, EscalationLevel::Normal);

        // Different tool (read_file), different error — resets because approach changed
        // AND the error sig is different (different tool name)
        ps.update("read_file", true, args2, Some(("shell", args1)));
        // Different tool + different error → not a fixation pattern
        assert_eq!(ps.level, EscalationLevel::Normal);
    }

    #[test]
    fn test_different_tool_name_resets_error_sig() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "bad"}));

        // Get to L2 with shell errors
        ps.update("shell", true, args_h, None);
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        assert_eq!(ps.level, EscalationLevel::ResearchRequired);

        // Switch to a different tool that also errors — new error sig starts fresh
        let new_args = hash_args(&serde_json::json!({"file": "missing.txt"}));
        ps.update("read_file", true, new_args, Some(("shell", args_h)));
        // First time this tool errors → Normal (new error pattern)
        assert_eq!(ps.level, EscalationLevel::Normal);
    }

    #[test]
    fn test_intervention_messages_per_level() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "test"}));

        // L0: no message
        assert!(ps.intervention_message().is_none());

        // L1: hint message
        ps.update("shell", true, args_h, None);
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        let msg = ps.intervention_message().unwrap();
        assert!(msg.contains("DIFFERENT tool"));

        // L2: research required
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        let msg2 = ps.intervention_message().unwrap();
        assert!(msg2.contains("Research Before Retrying"));
        assert!(msg2.contains("web_search"));

        // L3: forced divergence
        ps.update("shell", true, args_h, Some(("shell", args_h)));
        let msg3 = ps.intervention_message().unwrap();
        assert!(msg3.contains("Forced Divergence"));
        assert!(msg3.contains("fundamentally different"));
    }

    #[test]
    fn test_mark_researched() {
        let mut ps = ProactivityState::new();
        assert!(!ps.has_researched);
        ps.mark_researched();
        assert!(ps.has_researched);
    }

    #[test]
    fn test_successful_execution_does_not_escalate() {
        let mut ps = ProactivityState::new();
        let args_h = hash_args(&serde_json::json!({"cmd": "echo hello"}));

        // Successful execution repeated 5 times — no escalation
        for _ in 0..5 {
            ps.update("shell", false, args_h, Some(("shell", args_h)));
        }
        assert_eq!(ps.level, EscalationLevel::Normal);
    }

    #[test]
    fn test_trim_context_under_budget() {
        let mut msgs = vec![LlmMessage::system("system"), LlmMessage::user("hello")];
        trim::trim_context(&mut msgs, 1000);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_trim_context_over_budget() {
        let mut msgs = vec![
            LlmMessage::system("sys"),
            LlmMessage::user(&"x".repeat(2000)),
            LlmMessage::assistant(&"y".repeat(2000)),
            LlmMessage::user("recent1"),
            LlmMessage::assistant("recent2"),
            LlmMessage::user("latest"),
        ];
        let original_len = msgs.len();
        trim::trim_context(&mut msgs, 500);
        assert!(
            msgs.len() < original_len,
            "Should have trimmed some messages"
        );
        assert_eq!(msgs[0].role, LlmRole::System, "System prompt must survive");
    }

    #[test]
    fn test_agent_budget_config() {
        let agent = AgentLoop::new()
            .with_tool_result_budget(2000)
            .with_context_budget(50000);
        assert_eq!(agent.max_tool_result_chars, 2000);
        assert_eq!(agent.max_context_chars, 50000);
    }

    // ── run_subagent now DELEGATES to run_loop (unified engine, P0.1) ──
    // The thin collect wrapper must reconstruct the streamed response text and
    // return it — exactly like the old inline loop did.
    #[tokio::test]
    async fn test_run_subagent_delegates_to_loop() {
        use crate::llm::{MockLlmProvider, MockScript, MockStep};

        // Declarative mock pipeline: one text step answers the single LLM call.
        let mock = MockLlmProvider::from_script(
            MockScript::new().then(MockStep::Text("Final answer: 42".into())),
        );
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let result = AgentLoop::sub_agent(3)
            .run_subagent(
                llm,
                Arc::new(reg),
                vec![LlmMessage::user("solve it")],
                CancellationToken::new(),
            )
            .await;
        assert!(
            result.contains("Final answer: 42"),
            "run_subagent must collect streamed text through run_loop, got: {result:?}"
        );
    }

    // ── run_subagent collects the value across a tool-calling turn. ──
    // Tools execute, results re-inject, the next LLM turn commits the value.
    #[tokio::test]
    async fn test_run_subagent_collects_after_tool_call() {
        use crate::llm::{MockLlmProvider, MockScript, MockStep};

        // Mock pipeline: turn 1 calls `echo`, turn 2 commits the value.
        let mock = MockLlmProvider::from_script(
            MockScript::new()
                .then(MockStep::Calls(vec![(
                    "echo",
                    serde_json::json!({"text": "hello"}),
                )]))
                .then(MockStep::Text("Final answer: hello".into())),
        );
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let mock_arc = Arc::new(mock);
        let llm: Arc<dyn LlmProvider> = mock_arc.clone();
        let result = AgentLoop::sub_agent(3)
            .run_subagent(
                llm,
                Arc::new(reg),
                vec![LlmMessage::user("echo hello")],
                CancellationToken::new(),
            )
            .await;
        assert!(
            result.contains("Final answer: hello"),
            "run_subagent must collect the value across a tool-calling turn, got: {result:?}"
        );
        // The mock pipeline asserts the agent really invoked `echo` with the
        // scripted args (not just that a value came back).
        mock_arc.assert_calls_contain("echo", &serde_json::json!({"text": "hello"}));
    }
}
