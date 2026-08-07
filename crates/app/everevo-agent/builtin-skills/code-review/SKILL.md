---
name: code-review
description: Systematic code review across multiple dimensions. Finds bugs, security issues, and design problems before they reach production.
tools: [read_file, code_search, shell, list_dir]
when_to_use:
  - User asks for a code review
  - Before committing or merging code
  - Assessing pull request quality
  - Auditing security of new code
---

# Code Review

## What This Skill Does

Performs a multi-dimensional code review, examining correctness, security,
performance, maintainability, and test coverage. Each dimension is reviewed
independently, and findings are ranked by severity.

## Review Dimensions

### 1. Correctness
- Are there logic errors, off-by-one mistakes, or edge cases?
- Is error handling adequate for all failure modes?
- Are there race conditions or deadlocks in concurrent code?
- Does the code handle null/empty/error states correctly?

### 2. Security
- Are there injection vulnerabilities (SQL, command, path)?
- Are secrets/keys hardcoded or exposed?
- Is input validation comprehensive?
- Are authentication/authorization checks correct?

### 3. Performance
- Are there N+1 queries or unnecessary allocations?
- Are large data structures cloned when references would suffice?
- Are there blocking operations on async threads?
- Is caching used where appropriate?

### 4. Maintainability
- Are function/method responsibilities clear and single?
- Is the naming consistent with project conventions?
- Are there magic numbers or unexplained constants?
- Is the code adequately commented (why, not what)?

### 5. Test Coverage
- Are happy paths covered?
- Are error paths covered?
- Are edge cases covered (empty input, max values, nulls)?
- Are integration tests present for cross-module interactions?

## Output Format

For each finding, report:
- **Severity**: CRITICAL / HIGH / MEDIUM / LOW
- **File**: `path:line`
- **Issue**: one-sentence description
- **Fix**: concrete recommendation

## Rules

- Never flag style issues that `cargo fmt` / `prettier` would fix
- Never flag code you didn't read fully
- If a finding is plausible but unverified, mark it `[UNVERIFIED]`
- CRITICAL findings always require verification before reporting
