---
name: debug-error
description: Structured error debugging workflow. Stop guessing — collect evidence, form hypotheses, test systematically.
tools: [shell, read_file, code_search, web_search]
when_to_use:
  - User reports an error or bug
  - Build / test / CI failures
  - Runtime crashes or panics
  - Unexpected behavior in production
---

# Structured Debugging

## What This Skill Does

Prevents the "guess and check" debugging anti-pattern. Forces systematic
evidence collection, hypothesis formation, and targeted testing. Each step
must be completed before moving to the next.

## Workflow

### Phase 1: Reproduce (MANDATORY)
1. Collect the EXACT error message (copy-paste, no paraphrasing)
2. Note the environment: OS, tool versions, relevant config
3. Try to reproduce locally:
   - Same command with same arguments
   - Same input data
   - Same environment variables
4. If NOT reproducible: note the differences. The bug may be environment-specific.

### Phase 2: Isolate
1. Find the minimal reproduction:
   - Remove unrelated code / dependencies
   - Reduce input to smallest failing case
   - If the error disappears, binary-search the reduction
2. Identify the exact line or call that triggers the error
3. Check git log for recent changes to that file (`git log --oneline -10 -- <file>`)

### Phase 3: Diagnose
1. Form at least 3 hypotheses for WHY the error occurs
2. For each hypothesis, design a test that would confirm/refute it
3. Run the tests in order of likelihood
4. If ALL hypotheses are refuted, go back to Phase 2 with new evidence

### Phase 4: Fix
1. Write the fix (minimal change)
2. Write a regression test
3. Verify the test fails before the fix and passes after
4. Check for similar patterns elsewhere in the codebase

### Phase 5: Verify
1. Run the full test suite
2. Run clippy / linter
3. If the fix touches multiple files, run affected integration tests

## Anti-Patterns (BLOCKED)

- Trying random fixes without a hypothesis
- Changing multiple things at once
- "Let me update this dependency — maybe that fixes it"
- Ignoring the error message and guessing
- Flipping flags (--feature, cfg) without understanding why

## Search Strategy

When stuck:
1. `web_search("{exact error message}")` — always FIRST
2. `web_search("{error type} {language/framework} best practice")`
3. `code_search("error")` in the relevant module
4. Check project issue tracker if applicable
