---
name: write-tests
description: Write thorough, maintainable tests following project conventions. Covers unit, integration, and edge-case testing.
tools: [read_file, shell, code_search]
when_to_use:
  - User asks to write tests
  - New feature needs test coverage
  - Bug fix needs regression test
  - Improving test coverage of existing code
---

# Write Tests

## What This Skill Does

Guides test writing with project-specific conventions and coverage patterns.
Ensures tests are thorough, readable, and maintainable — not just coverage
metrics.

## Before Writing

1. Read the code being tested — understand the interface and behavior
2. Check existing tests in the project for conventions:
   - Test module naming (`#[cfg(test)] mod tests`)
   - Helper/utility patterns
   - Mock/stub approaches
3. Identify all code paths:
   - Happy path (normal input → expected output)
   - Error paths (invalid input → appropriate error)
   - Edge cases (empty, max, boundary values)
   - Async/blocking boundaries (tokio tests)

## Test Structure (Rust)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_<scenario>_<expected_behavior>() {
        // Arrange: set up inputs and state
        // Act: call the function under test
        // Assert: verify the result
    }
}
```

## Test Structure (TypeScript)

```typescript
describe('<module>', () => {
  it('should <behavior> when <scenario>', () => {
    // Arrange
    // Act
    // Assert
  });
});
```

## Coverage Checklist

For every public function/method:
- [ ] Normal input produces expected output
- [ ] Empty / zero / null input handled gracefully
- [ ] Maximum / boundary values work correctly
- [ ] Error states return appropriate errors (not panics)
- [ ] Concurrent / parallel usage is safe (if applicable)

## Rules

- Test names describe the scenario AND expected behavior
- Each test verifies ONE thing (one assertion concept per test)
- Use realistic data — not "foo", "bar", 123
- Prefer `assert_eq!` over `assert!` for better failure messages
- Don't test implementation details — test observable behavior
- If mocking is needed, mock at the module boundary, not internally
- Integration tests go in `tests/` directory (Rust) or `__tests__/` (TS)
