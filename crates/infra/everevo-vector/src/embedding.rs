//! Embedding model trait and implementations.

use everevo_core::EverEvoError;

/// Abstract embedding model — swap implementations without changing call sites.
pub trait EmbeddingModel: Send + Sync {
    /// Encode a single text into a vector.
    fn encode(&self, text: &str) -> Result<Vec<f32>, EverEvoError>;
    /// Encode multiple texts in batch.
    fn encode_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EverEvoError> {
        texts.iter().map(|t| self.encode(t)).collect()
    }
    /// Dimensionality of the output vectors.
    fn dimension(&self) -> usize;
}

// Blanket impl: allows `Box<dyn EmbeddingModel>` to be used as `EmbeddingModel`
impl<T: EmbeddingModel + ?Sized> EmbeddingModel for Box<T> {
    fn encode(&self, text: &str) -> Result<Vec<f32>, EverEvoError> {
        (**self).encode(text)
    }
    fn encode_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EverEvoError> {
        (**self).encode_batch(texts)
    }
    fn dimension(&self) -> usize {
        (**self).dimension()
    }
}

// ── Dummy Embedder ────────────────────────────────────────────────────────

/// Fallback embedder — returns a zero vector.
/// Used when no embedding model is configured. Enables development and
/// testing without requiring ONNX runtime or GPU.
pub struct DummyEmbedder {
    dim: usize,
}

impl DummyEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl EmbeddingModel for DummyEmbedder {
    fn encode(&self, _text: &str) -> Result<Vec<f32>, EverEvoError> {
        Ok(vec![0.0_f32; self.dim])
    }
    fn dimension(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dummy_embedder() {
        let emb = DummyEmbedder::new(384);
        let v = emb.encode("hello").unwrap();
        assert_eq!(v.len(), 384);
        assert_eq!(v[0], 0.0);
    }
}
