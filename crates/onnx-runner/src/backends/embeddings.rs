use crate::hub::ModelHandle;
use anyhow::{bail, Context, Result};
use ndarray::Array2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::Arc;
use tokenizers::Tokenizer;
use tracing::debug;

/// Configuration for the EmbeddingsOnnx backend.
#[derive(Debug, Clone)]
pub struct EmbeddingsConfig {
    /// HuggingFace repo id, e.g. `"onnx-community/embeddinggemma-300m-ONNX"`.
    pub repo_id: String,
    /// Prefer the quantized ONNX variant when available.
    pub prefer_quantized: bool,
    /// Maximum token sequence length. Inputs are truncated/padded to this length.
    pub max_seq_len: usize,
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            repo_id: "onnx-community/embeddinggemma-300m-ONNX".into(),
            prefer_quantized: true,
            max_seq_len: 512,
        }
    }
}

/// Output from a single embedding inference call.
#[derive(Debug, Clone)]
pub struct EmbeddingsOutput {
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// Model generation token — `"{repo}@{sha8}"`. Consumers should store this
    /// alongside the vector so they can detect embedding-space drift on model swap.
    pub model_gen: String,
    /// Embedding dimension (== `vector.len()`).
    pub dim: usize,
}

/// Local ONNX embedding backend.
///
/// Loads `onnx-community/embeddinggemma-300m-ONNX` (or any compatible
/// sentence-embedding ONNX model) and runs mean-pooled inference.
pub struct EmbeddingsBackend {
    /// Session held in a Mutex to satisfy ort v2's `&mut self` run requirement.
    session: Arc<std::sync::Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
    model_gen: String,
    max_seq_len: usize,
}

impl EmbeddingsBackend {
    /// Load the backend from a resolved [`ModelHandle`].
    pub fn load(handle: &ModelHandle, max_seq_len: usize) -> Result<Self> {
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("failed to create ONNX session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("failed to set graph optimisation level: {}", e))?
            .commit_from_file(&handle.model_path)
            .with_context(|| {
                format!(
                    "failed to load ONNX model from {}",
                    handle.model_path.display()
                )
            })?;

        let tokenizer = Tokenizer::from_file(&handle.tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {}", e))?;

        tracing::info!(
            model_gen = %handle.model_gen,
            model_path = %handle.model_path.display(),
            "EmbeddingsBackend loaded"
        );

        Ok(Self {
            session: Arc::new(std::sync::Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
            model_gen: handle.model_gen.clone(),
            max_seq_len,
        })
    }

    /// Embed a single text string. Returns an [`EmbeddingsOutput`] containing
    /// the mean-pooled vector and the `model_gen` provenance token.
    pub fn embed(&self, text: &str) -> Result<EmbeddingsOutput> {
        let (input_ids, attention_mask) = self.tokenize(text)?;
        let seq_len = input_ids.len();

        // Build i64 tensors shaped [1, seq_len] for ONNX Runtime.
        let ids_data: Vec<i64> = input_ids.iter().map(|&x| x as i64).collect();
        let mask_data: Vec<i64> = attention_mask.iter().map(|&x| x as i64).collect();

        let ids_array = Array2::from_shape_vec((1, seq_len), ids_data)
            .context("failed to build input_ids array")?;
        let mask_array = Array2::from_shape_vec((1, seq_len), mask_data)
            .context("failed to build attention_mask array")?;

        let ids_tensor = Tensor::<i64>::from_array(ids_array)
            .map_err(|e| anyhow::anyhow!("failed to create input_ids tensor: {}", e))?;
        let mask_tensor = Tensor::<i64>::from_array(mask_array)
            .map_err(|e| anyhow::anyhow!("failed to create attention_mask tensor: {}", e))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session mutex poisoned"))?;

        // Check if model expects token_type_ids by inspecting input names
        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();
        let needs_token_type_ids = input_names.iter().any(|name| name == "token_type_ids");

        let outputs = if needs_token_type_ids {
            // token_type_ids: all zeros for single-segment embeddings
            let token_type_data: Vec<i64> = vec![0i64; seq_len];
            let token_type_array = Array2::from_shape_vec((1, seq_len), token_type_data)
                .context("failed to build token_type_ids array")?;
            let token_type_tensor = Tensor::<i64>::from_array(token_type_array)
                .map_err(|e| anyhow::anyhow!("failed to create token_type_ids tensor: {}", e))?;

            session
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "attention_mask" => mask_tensor,
                    "token_type_ids" => token_type_tensor,
                ])
                .map_err(|e| anyhow::anyhow!("ONNX session run failed: {}", e))?
        } else {
            session
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "attention_mask" => mask_tensor,
                ])
                .map_err(|e| anyhow::anyhow!("ONNX session run failed: {}", e))?
        };

        // Output is `last_hidden_state`: [batch=1, seq_len, hidden_dim].
        // Use try_extract_tensor which returns (&Shape, &[T]) in ort v2.
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("failed to extract hidden state tensor: {}", e))?;

        // Shape implements Deref<Target = [i64]>
        let dims: &[i64] = shape;
        if dims.len() != 3 {
            bail!(
                "expected 3D hidden state [batch, seq, hidden], got {}D",
                dims.len()
            );
        }
        let hidden_dim = dims[2] as usize;

        let vector = mean_pool_flat(data, seq_len, hidden_dim, &attention_mask);
        let dim = vector.len();

        debug!(dim, model_gen = %self.model_gen, "embedding produced");

        Ok(EmbeddingsOutput {
            vector,
            model_gen: self.model_gen.clone(),
            dim,
        })
    }

    /// The model generation token for the currently loaded model.
    pub fn model_gen(&self) -> &str {
        &self.model_gen
    }

    /// Tokenize `text` and return `(input_ids, attention_mask)` truncated to
    /// `max_seq_len`.
    fn tokenize(&self, text: &str) -> Result<(Vec<u32>, Vec<u32>)> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenisation failed: {}", e))?;

        let ids: Vec<u32> = encoding
            .get_ids()
            .iter()
            .take(self.max_seq_len)
            .copied()
            .collect();

        let mask: Vec<u32> = encoding
            .get_attention_mask()
            .iter()
            .take(self.max_seq_len)
            .copied()
            .collect();

        Ok((ids, mask))
    }
}

/// Mean-pool the last hidden state over non-padding token positions.
///
/// `data`: flat row-major slice of shape `[1, seq_len, hidden_dim]`
/// `mask`: length `seq_len`, values 0 or 1
fn mean_pool_flat(data: &[f32], seq_len: usize, hidden_dim: usize, mask: &[u32]) -> Vec<f32> {
    let mut sum = vec![0.0f32; hidden_dim];
    let mut count = 0u32;

    for i in 0..seq_len {
        if i < mask.len() && mask[i] != 0 {
            let offset = i * hidden_dim;
            for j in 0..hidden_dim {
                sum[j] += data[offset + j];
            }
            count += 1;
        }
    }

    if count > 0 {
        let inv = 1.0 / count as f32;
        sum.iter_mut().for_each(|v| *v *= inv);
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::mean_pool_flat;

    #[test]
    fn mean_pool_ignores_padding() {
        // Two real tokens, third is padding. Hidden dim = 2.
        let data = vec![1.0f32, 2.0, 3.0, 4.0, 99.0, 99.0];
        let mask = vec![1u32, 1, 0];
        let result = mean_pool_flat(&data, 3, 2, &mask);
        assert_eq!(result, vec![2.0, 3.0]);
    }

    #[test]
    fn mean_pool_all_active() {
        let data = vec![2.0f32, 4.0, 4.0, 8.0];
        let mask = vec![1u32, 1];
        let result = mean_pool_flat(&data, 2, 2, &mask);
        assert_eq!(result, vec![3.0, 6.0]);
    }

    #[test]
    fn mean_pool_all_padding_returns_zeros() {
        let data = vec![5.0f32, 5.0];
        let mask = vec![0u32];
        let result = mean_pool_flat(&data, 1, 2, &mask);
        assert_eq!(result, vec![0.0, 0.0]);
    }
}
