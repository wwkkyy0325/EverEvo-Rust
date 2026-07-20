# Thinking / Reasoning Architecture

## Two Mechanisms, One UI

EverEvo distinguishes two sources of model "thinking," but presents them through a single unified UI.

### Layer 1: Model-Native Thinking (implemented)

```
User Input → [Internal reasoning tokens] → [Output tokens]
              ↑ thinking delta channel     ↑ text delta channel
```

Built into reasoning models at the architecture level (RL-trained, test-time compute).
The model can self-correct, backtrack, and try alternative reasoning paths internally.

| Provider | Format | SSE Channel | Cost |
|----------|--------|-------------|------|
| DeepSeek V4 Pro | Anthropic-compatible | `content_block_delta` → `delta.thinking` | Same as output ($0.87/M) |
| DeepSeek V4 Pro | OpenAI-compatible | `delta.reasoning_content` | Same as output |
| Claude Opus 4.5+ | Anthropic native | `thinking_delta` content blocks | Output rate ($15-25/M) |
| GPT-5 | OpenAI | `delta.reasoning_content` | Output rate |

**Key insight**: DeepSeek charges the same rate for thinking tokens as regular output — reasoning is effectively free. Claude charges full output rates, making extended thinking 5-15× more expensive per token than a non-thinking call.

### Layer 2: Application-Level Draft (planned, not yet implemented)

```
System Prompt: "Write your analysis in <draft>...</draft>, then answer in <answer>...</answer>"
Model Output:   "<draft>\nLet me analyze...\n</draft>\n<answer>\nThe answer is...\n</answer>"
```

A prompt-level pattern: instruct the model to write its reasoning as structured output before the final answer. Works with **any** model (not just reasoning models), but the thinking IS output text — it cannot be internally revised.

**Use case**: Non-reasoning models (DeepSeek Flash, Claude Haiku, GPT-4o-mini) where native thinking is unavailable or too expensive.

**Cost**: Full output token rate for the draft content.

### Comparison

| | Native Thinking | Prompt Draft |
|---|---|---|
| Self-correction | ✅ Can backtrack | ❌ Commit-on-write |
| Token cost | DeepSeek: same rate / Claude: output rate | Always output rate |
| Model support | Reasoning models only | All models |
| Format control | None (API-defined) | Full (prompt-defined) |
| Stability | Provider-dependent (DeepSeek Anthropic endpoint has known bugs) | Fully controlled |

### Automatic Fallback Strategy

```
if model supports native thinking:
    → use Layer 1 (delta.thinking / reasoning_content)
else:
    → use Layer 2 (prompt-instructed <draft> tags)

Both → rendered in the same collapsible thinking panel
```

## UI Design: Same Bubble, Different Label

**Decision**: One thinking panel, two labels. Not two separate bubble types.

**Rationale**:
- **User mental model**: "The AI is thinking" — the mechanism is irrelevant to the user
- **Visual consistency**: Same purple collapsible panel, same expand/collapse behavior
- **Reduced cognitive load**: One interaction pattern to learn
- **Code simplicity**: Single component, single code path, composable data source

| Thinking Source | Panel Label | Visual Style |
|---|---|---|
| Model-native (reasoning model) | `🧠 深度思考` | Purple, italic, auto-collapse on answer |
| Prompt draft (any model) | `📝 分析草稿` | Same purple, same behavior |

### Interaction Model

```
┌─────────────────────────────────────────────┐
│ ▼ 🧠 深度思考 · 1,247字                       │  ← Collapsed by default
├─────────────────────────────────────────────┤
│ Let me analyze this problem step by step...  │  ← Purple italic text
│                                              │
│ 1. First, I need to understand the user's    │
│    requirement...                            │
│ 2. Then consider the constraints...          │
│ 3. Finally, synthesize the answer...         │
└─────────────────────────────────────────────┘
│                                              │
│ Here is my answer: ...                       │  ← Normal white text
└─────────────────────────────────────────────┘
```

### Streaming Behavior

1. **Thinking phase begins**: Panel auto-opens, shows `🧠 思考中...` with live token count
2. **Answer phase begins**: Panel auto-collapses, thinking content preserved for review
3. **User clicks collapsed header**: Panel re-expands to show full reasoning
4. **Session switch**: Thinking content reset for the new session

## Cost Analysis

Concrete example: 20K system prompt, 100 calls × 200 user tokens + 500 thinking + 300 output tokens.

| Model | Per-Session Cost | Thinking Included? |
|-------|-----------------|-------------------|
| DeepSeek V4 Pro | ~$0.53 | Yes (output rate) |
| Claude Opus 4.8 | ~$10.00 | Yes (output rate) |
| DeepSeek V4 Flash | ~$0.17 | No (no native thinking) |

**Current recommendation**: DeepSeek V4 Pro via Anthropic-compatible endpoint for primary model — best reasoning quality per dollar.

## References

- [Claude Extended Thinking docs](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
- [Anthropic "think" tool announcement](https://www.anthropic.com/engineering/claude-think-tool)
- [DeepSeek Thinking Mode docs](https://api-docs.deepseek.com/guides/thinking_mode)
- [DeepSeek V4 Pricing](https://api-docs.deepseek.com/quick_start/pricing/)
- [NousResearch Hermes Agent: DeepSeek Anthropic endpoint fixes](https://github.com/NousResearch/hermes-agent/pull/17223)
