//! Mood embeddings: the bge-small-en-v1.5 int8 ONNX model and its tokenizer are
//! bundled into the binary at build time (`build.rs` compiles the ONNX weights
//! into `OUT_DIR/model/bge_small.rs`; the tokenizer is `include_bytes!`-d).
//! Inference runs via Burn + burn-onnx and returns 384-dim sentence embeddings.
//!
//! Loading is a one-time, infallible-in-practice operation guarded by a
//! `OnceLock`; a failure to load the bundled model is a build/runtime invariant
//! violation and panics (`global_embedder`).

use std::sync::OnceLock;

use anyhow::{Context, Result};
use burn::tensor::{Int, Tensor, TensorData};
use burn_flex::Flex;
use tokenizers::Tokenizer;

type Backend = Flex;

pub mod bge_small {
    include!(concat!(env!("OUT_DIR"), "/model/bge_small.rs"));
}

pub mod saliency_adaptor {
    include!(concat!(env!("OUT_DIR"), "/model/saliency_adaptor.rs"));
}

/// Dimensionality of bge-small-en-v1.5 sentence embeddings.
pub const EMBED_DIM: usize = 384;
/// Model supports up to 256 tokens; the model's own truncation cap.
const MAX_SEQ_LEN: usize = 256;

static TOKENIZER_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tokenizer.json"));

/// A loaded embedding model: Burn model + WordPiece tokenizer + Saliency Adaptor.
pub struct Embedder {
    model: bge_small::Model<Backend>,
    saliency_model: saliency_adaptor::Model<Backend>,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// Load the bundled embedding model and tokenizer.
    pub fn load() -> Result<Self> {
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_BYTES).map_err(|e| {
            anyhow::anyhow!("Failed to load embedded tokenizer: {e}")
        })?;
        let model = bge_small::Model::default();
        let saliency_model = saliency_adaptor::Model::default();

        Ok(Self {
            model,
            saliency_model,
            tokenizer,
        })
    }

    /// Compute the sentence embedding for `text`.
    ///
    /// `text` is trimmed before tokenization. `prepend` is concatenated to
    /// the trimmed text before embedding (use a non-empty prefix like
    /// `"feeling "` to anchor every embedding into the same lexical
    /// neighbourhood — the MiniLM space is wide, and a stray noun like
    /// `"happy"` would otherwise drift away from a `"great mood"` usage).
    /// Pass `""` for raw text-mode embedding (e.g. diagnostic `:embed`).
    ///
    /// Returns a unit-length (L2-normalized) vector of `EMBED_DIM` floats.
    pub fn embed(&self, text: &str, prepend: &str) -> Result<Vec<f32>> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            // Pad-only input would be meaningless; return a zero vector.
            return Ok(vec![0.0; EMBED_DIM]);
        }
        let text = format!("{prepend}{trimmed}");

        // Tokenize with truncation to the model's max sequence length.
        let mut tokenizer = self.tokenizer.clone();
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_SEQ_LEN,
                strategy: tokenizers::TruncationStrategy::LongestFirst,
                direction: tokenizers::TruncationDirection::Right,
                stride: 0,
            }))
            .map_err(|e| anyhow::anyhow!("Failed to set truncation: {e}"))?;
        let enc = tokenizer
            .encode(text.as_str(), true)
            .map_err(|e| anyhow::anyhow!("Failed to tokenize {:?}: {e}", text.as_str()))?;

        let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&m| m as i64).collect();
        let type_ids: Vec<i64> = enc.get_type_ids().iter().map(|&t| t as i64).collect();
        let seq_len = ids.len();

        let device = Default::default();

        let input_ids =
            Tensor::<Backend, 2, Int>::from_data(TensorData::new(ids, [1, seq_len]), &device);
        let attention_mask =
            Tensor::<Backend, 2, Int>::from_data(TensorData::new(mask, [1, seq_len]), &device);
        let token_type_ids =
            Tensor::<Backend, 2, Int>::from_data(TensorData::new(type_ids, [1, seq_len]), &device);

        let outputs = self
            .model
            .forward(input_ids, attention_mask, token_type_ids);
        let last_hidden_flat: Vec<f32> = outputs.into_data().to_vec::<f32>().unwrap();

        // CLS pooling: bge-small-en-v1.5 is trained to use the hidden state of
        // the first token ([CLS]) as the sentence embedding — the BAAI model
        // card's `sentence_embeddings = model_output[:, 0]`. Mean pooling was
        // the MiniLM/sentence-transformers convention and is NOT the bge
        // usage; the model card and Cloudflare's serving notes both recommend
        // cls pooling for better accuracy. The tokenizer always emits [CLS] at
        // index 0 (add_special_tokens = true), so the sentence vector is
        // `last_hidden_flat[0..EMBED_DIM]`.
        let mut pooled = last_hidden_flat[0..EMBED_DIM].to_vec();

        // L2 normalize
        let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for v in &mut pooled {
                *v /= norm;
            }
        }

        Ok(pooled)
    }

    /// Predict scalar emotional saliency in [0.0, 1.0] for a raw (unprefixed) embedding.
    pub fn predict_saliency(&self, raw_embedding: &[f32]) -> f32 {
        let device = Default::default();
        let input = Tensor::<Backend, 2>::from_data(
            TensorData::new(raw_embedding.to_vec(), [1, EMBED_DIM]),
            &device,
        );
        let output = self.saliency_model.forward(input);
        let val: Vec<f32> = output.into_data().to_vec::<f32>().unwrap();
        if val.is_empty() {
            0.0
        } else {
            val[0].clamp(0.0, 1.0)
        }
    }
}

/// Read a raw BLOB stored by [`embed_to_blob`] back into a vector.
pub fn blob_to_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() != EMBED_DIM * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(EMBED_DIM);
    for chunk in blob.chunks_exact(4) {
        out.push(f32::from_le_bytes(chunk.try_into().ok()?));
    }
    Some(out)
}

/// Serialize an embedding vector into a raw little-endian BLOB for SQLite.
pub fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|v| v.to_le_bytes()).collect()
}

static EMBEDDER: OnceLock<Embedder> = OnceLock::new();

/// Load the bundled embedding model once and return a reference to it.
///
/// The model and tokenizer are compiled into the binary, so loading cannot
/// be deferred or declined: a failure here means the binary is broken (corrupt
/// build artifact), and we panic rather than degrade silently.
pub fn global_embedder() -> &'static Embedder {
    EMBEDDER.get_or_init(|| match Embedder::load() {
        Ok(e) => e,
        Err(e) => panic!("Embedding model failed to load: {e:#}"),
    })
}

/// Serialize an embedding vector as a line of space-separated floats.
pub fn format_vector(v: &[f32]) -> String {
    v.iter()
        .map(|x| format!("{x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a line of space-separated floats back into a vector.
pub fn parse_vector(s: &str) -> Result<Vec<f32>> {
    let vals = s
        .split_whitespace()
        .map(|tok| {
            tok.parse::<f32>()
                .with_context(|| format!("Invalid float in vector line: {tok:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if vals.is_empty() {
        anyhow::bail!("Empty vector line");
    }
    Ok(vals)
}

/// Normalize a vector to unit length (no-op on zero vectors).
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        v.iter().map(|x| x / norm).collect()
    } else {
        v.to_vec()
    }
}

/// Dot product of two equal-length vectors.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Query SQLite `embedding_cache` for `text`. On miss, compute `embedder.embed(text, prefix)`
/// and persist the resulting BLOB to `embedding_cache`.
pub async fn get_or_embed_cached(
    pool: &sqlx::SqlitePool,
    embedder: &Embedder,
    text: &str,
    prefix: &str,
) -> Result<Vec<f32>> {
    let key = format!("{prefix}{text}");

    if let Ok(Some(blob)) = crate::sql::get_embedding_cache(pool, &key).await {
        if let Some(vec) = blob_to_embedding(&blob) {
            return Ok(vec);
        }
    }

    let vec = embedder.embed(text, prefix)?;
    let blob = embedding_to_blob(&vec);

    let _ = crate::sql::set_embedding_cache(pool, &key, &blob).await;

    Ok(vec)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip() {
        let v: Vec<f32> = (0..EMBED_DIM).map(|i| i as f32 * 0.5).collect();
        let blob = embedding_to_blob(&v);
        assert_eq!(blob.len(), EMBED_DIM * 4);
        let back = blob_to_embedding(&blob).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn blob_rejects_wrong_len() {
        assert!(blob_to_embedding(&[0u8; 10]).is_none());
        assert!(blob_to_embedding(&[]).is_none());
    }

    #[test]
    fn vector_roundtrip() {
        let v: Vec<f32> = (0..5).map(|i| i as f32 * 0.25).collect();
        let s = format_vector(&v);
        assert_eq!(parse_vector(&s).unwrap(), v);
    }

    #[test]
    fn parse_vector_rejects_garbage() {
        assert!(parse_vector("").is_err());
        assert!(parse_vector("1.0 abc 2.0").is_err());
    }

    #[test]
    fn normalize_unit_length() {
        let v = normalize(&[3.0, 4.0]);
        assert!((v[0].powi(2) + v[1].powi(2) - 1.0).abs() < 1e-5);
        assert_eq!(normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn dot_product() {
        assert_eq!(dot(&[1.0, 2.0], &[3.0, 4.0]), 11.0);
    }
}
