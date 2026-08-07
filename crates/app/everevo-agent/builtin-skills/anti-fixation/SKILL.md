---
name: anti-fixation
description: Detects fixation loops and forces alternative approaches. Invoke manually or let the agent loop auto-activate.
tools: [web_search, web_fetch]
when_to_use:
  - Agent is stuck repeating the same failed approach
  - User says the agent is going in circles
  - Same tool returns same error 3+ times
---

# Anti-Fixation Protocol

## What This Skill Does

Prevents the agent from getting stuck in "fixation loops" — repeatedly
trying the same approach with minor variations and expecting different
results. When activated, it forces the agent to STEP BACK, research
alternatives, and choose a fundamentally different path.

## When to Use

Invoke `/anti-fixation` when you see the agent:
- Trying the same command 3+ times with minor tweaks
- Saying "let me try one more time" or "I'll adjust the parameters"
- Blaming the environment instead of changing approach
- Refusing to use web_search or web_fetch for research

## Activation Behavior

When this skill triggers:

### Phase 1: Stop & Assess
1. **STOP all retries immediately.**
2. State clearly: "Anti-fixation protocol activated — switching approach."
3. List what you have tried so far (tool + approach + error).

### Phase 2: Research (Mandatory)
1. Call `web_search` for at least 2 queries:
   - The exact error message
   - "{task description} alternative approach {language/framework}"
2. Read any promising results via `web_fetch`.
3. Document what you found.

### Phase 3: Divergent Thinking
1. List at least 3 HYPOTHESES for why the current approach failed.
2. For each hypothesis, propose an alternative approach.
3. Approaches MUST differ at the architectural level:
   - ❌ "Change the parameter from 5 to 10"
   - ✅ "Use library X instead of hand-rolling"
   - ✅ "Switch from polling to event-driven"
   - ✅ "Use a different algorithm entirely"

### Phase 4: Choose & Execute
1. Pick the most promising alternative based on research.
2. Explain WHY this approach should work where the previous one failed.
3. Execute. If this also fails, loop back to Phase 2 with the new failure.

## Anti-Patterns (Blocked Behaviors)

When this skill is active, the following are BLOCKED:
- Retrying the same tool with only parameter changes
- "Let me try again with more error handling"
- "Maybe it's an environment issue" (not allowed without evidence)
- Giving up and saying "I can't solve this"

## Library-First Principle

Before hand-rolling any non-trivial utility:
1. Search: `web_search("{task} {language} crate/package")`
2. If a maintained solution exists: USE IT.
3. If no solution exists: document why in a code comment.
4. Never implement from scratch what a maintained library already provides.

## Guardrails

- Maximum 2 iterations through Phase 2-4 before spawning a sub-agent
  with fresh context (different model if available)
- Each research phase MUST produce at least one new piece of information
- Skill deactivates automatically when the agent successfully changes approach
