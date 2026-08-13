//! Tool-result deduplication — collapses near-identical results in one turn.
//! Extracted from driver.rs during the 2026-08-13 physical restructure.

use super::proactivity::hash_str;

/// When N tool results in the same turn are near-identical (e.g. 3 sub-agents
/// all reporting the same path inconsistency bug), keep the first 2 and replace
/// the rest with a collapsed summary. This prevents flooding the LLM context
/// with duplicate observations that cause repetition loops in the thinking output.
pub(crate) fn deduplicate_tool_results(
    results: &mut [(String, String, Vec<everevo_core::ImageData>)],
) {
    if results.len() < 3 {
        return;
    }

    // Phase 1: fingerprint each result
    let fingerprints: Vec<u64> = results
        .iter()
        .map(|(_, content, _)| {
            let prefix: String = content.chars().take(200).collect();
            hash_str(&prefix)
        })
        .collect();

    // Phase 2: find groups with high similarity
    let mut seen: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    for (i, &fp) in fingerprints.iter().enumerate() {
        seen.entry(fp).or_default().push(i);
    }

    // Phase 3: collapse groups with >2 members
    for indices in seen.values() {
        if indices.len() <= 2 {
            continue;
        }
        let keep_id = results[indices[0]].0.clone();
        let dup_count = indices.len() - 2;
        for &idx in &indices[2..] {
            results[idx] = (
                results[idx].0.clone(),
                format!(
                    "(duplicate of {keep_id} — {dup_count} similar results collapsed to save context)"
                ),
                Vec::new(),
            );
        }
    }
}
