# ADR-000: Decision Title

**Status:** proposed | accepted | deprecated | superseded
**Date:** YYYY-MM-DD
**Deciders:** [who was involved]

---

## Context

What problem are we solving? What constraints exist?

## Decision

What did we decide? Be specific.

## Alternatives Considered

| Option | Pros | Cons | Why Rejected |
|--------|------|------|-------------|
| A | ... | ... | ... |
| B (chosen) | ... | ... | — |

## Consequences

What becomes easier? What becomes harder?

## Affected Interfaces

- [ ] `everevo-core::Tool::execute` — signature changed (added `cancel` param)
- [ ] `frontend/src/store.ts:MessageItem` — new field `blocks`
- [ ] (list all public API changes)

## Migration Path

How do existing consumers adapt? Is backward compatibility maintained?
