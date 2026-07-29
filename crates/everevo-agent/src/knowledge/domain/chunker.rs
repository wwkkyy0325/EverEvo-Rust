//! Semantic chunker — splits text at natural topic boundaries.
//!
//! Based on TopoChunker (arXiv:2603.18409) semantic breakpoint detection.

use super::document::ChunkType;

/// Semantic percentile chunker — splits text at natural topic boundaries.
pub struct SemanticChunker {
    pub target_chunk_size: usize,
    pub overlap_size: usize,
    pub percentile: f64, // 95th percentile for semantic breakpoint
}

impl Default for SemanticChunker {
    fn default() -> Self {
        Self {
            target_chunk_size: 500,
            overlap_size: 50,
            percentile: 0.95,
        }
    }
}

impl SemanticChunker {
    /// Split text into semantically coherent chunks.
    /// Uses simple paragraph/sentence boundary detection.
    /// Phase 3b upgrades to embedding-distance-based semantic breaks.
    pub fn chunk(&self, text: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let paragraphs: Vec<&str> = text.split("\n\n").collect();
        let mut current = String::new();

        for para in paragraphs {
            let para = para.trim();
            if para.is_empty() {
                continue;
            }

            if current.len() + para.len() > self.target_chunk_size && !current.is_empty() {
                chunks.push(current.trim().to_string());
                // Keep overlap: last few sentences of previous chunk
                let overlap = Self::tail_sentences(&current, self.overlap_size);
                current = overlap;
            }
            if !current.is_empty() && !current.ends_with('\n') {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }

        if !current.trim().is_empty() {
            chunks.push(current.trim().to_string());
        }

        chunks
    }

    fn tail_sentences(text: &str, max_chars: usize) -> String {
        if text.len() <= max_chars {
            return text.to_string();
        }
        let tail = &text[text.len() - max_chars..];
        // Find first complete sentence boundary
        for (i, c) in tail.char_indices() {
            if c == '.' || c == '。' || c == '\n' {
                return tail[i..].trim().to_string();
            }
        }
        tail.to_string()
    }

    /// Detect chunk type from content.
    pub fn detect_chunk_type(content: &str) -> ChunkType {
        if content.starts_with("```") {
            ChunkType::Code
        } else if content.starts_with('#') {
            ChunkType::Heading
        } else if content.contains("|---|---|") {
            ChunkType::Table
        } else {
            ChunkType::Text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_chunker() {
        let chunker = SemanticChunker::default();
        let text = "Paragraph one.\n\nParagraph two.\n\nParagraph three.";
        let chunks = chunker.chunk(text);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunker_multiple_paragraphs() {
        let mut chunker = SemanticChunker::default();
        chunker.target_chunk_size = 20;
        let text = "Para one here.\n\nPara two here.\n\nPara three here.\n\nPara four here.";
        let chunks = chunker.chunk(&text);
        assert!(
            chunks.len() >= 2,
            "Expected at least 2 chunks, got {}",
            chunks.len()
        );
    }
}
