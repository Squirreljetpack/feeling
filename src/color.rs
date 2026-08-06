#![allow(clippy::needless_range_loop)]
//! Mood color computation via NNLS basis-ray regression & saliency scaling.
//!
//! Mood strings are embedded with the nomic-embed-text-v1.5 model, projected onto a
//! user-defined set of basis `MoodEndpoint`s via Non-Negative Least Squares (NNLS),
//! filtered by contribution %, blended using power-weighted centroid mixing in Oklab,
//! and rescaled by predicted emotional saliency, gated by the configured
//! `emotional_saliency_gate` P (Seff = 1 + P*(S - 1)):
//!
//! L_final = L_neutral + Seff * (L_blended - L_neutral)
//! a_final = Seff * a_blended
//! b_final = Seff * b_blended

use std::collections::HashMap;

use anyhow::Result;
use oklab::Oklab;
use sqlx::SqlitePool;

use crate::color_conversion::rgb_to_oklab;
use crate::config::{ColorAxesSettings, MoodEndpoint};
use crate::embed::Embedder;
use crate::sql::FeelingRow;
use crate::utils::Percentage;

/// State for a single basis mood ray.
#[derive(Debug, Clone)]
pub struct BasisMood {
    pub mood: String,
    pub oklab: Oklab,
    pub vector: Vec<f32>,
}

/// Precomputed state for mood-color regression & blending.
#[derive(Debug, Clone)]
pub struct ColorAxes {
    pub basis_moods: Vec<BasisMood>,
    pub base_vector: Vec<f32>,
    pub steepness: f32,
    pub min_contribution: Percentage,
    pub top_k: usize,
    pub baseline_oklab_l: Percentage,
    /// Gate P on emotional saliency: effective saliency Seff = 1 + P*(S - 1).
    pub emotional_saliency_gate: Percentage,
    /// Text anchor prefixed to a mood before embedding ("person says: "), so
    /// the embedding encodes the mood as a statement.
    pub prefix_string: String,
    /// Text used as the neutral baseline anchor subtracted when computing basis ray shift vectors.
    pub base_string: String,
    /// Precomputed Gram matrix (A^T A) of dot products between basis mood vectors.
    pub gram_matrix: Vec<Vec<f32>>,
}

/// NNLS regression output for one embedding: the contributing basis moods
/// with their raw NNLS weights, the rescaled weights used for blending, and
/// the predicted emotional saliency of the mood text.
#[derive(Debug)]
pub struct MoodWeights {
    /// (basis mood index, raw NNLS weight), filtered by `min_contribution`,
    /// sorted descending, truncated to `top_k` — same order as [`Self::rescaled`].
    pub raw: Vec<(usize, f32)>,
    /// Power-weighted rescale of `raw`, normalized to sum 1.
    pub rescaled: Vec<f32>,
    /// Predicted emotional saliency S in [0, 1] for the mood text (1.0 when
    /// the text is empty or the prediction fails).
    pub saliency: f32,
}

/// Predict the emotional saliency score for un-prefixed raw mood text,
/// falling back to 1.0 on any failure (embedding, session run, extraction).
/// Shared by [`ColorAxes::regression_weights`] and entry creation
/// (`handle_entry` computes the score at insert time).
pub fn predict_saliency(embedder: &Embedder, mood_text: &str) -> f32 {
    let trimmed_text = mood_text.trim();
    if trimmed_text.is_empty() {
        return 1.0;
    }
    match embedder
        .embed(trimmed_text, "")
        .and_then(|raw_emb| embedder.predict_saliency(&raw_emb))
    {
        Ok(s) => s,
        Err(err) => {
            log::error!("Saliency prediction failed for {:?}: {err:#}", trimmed_text);
            1.0
        }
    }
}

impl ColorAxes {
    /// Build basis vectors from the given endpoint pairs using SQLite cached
    /// embeddings. The `color_axes` cache check lives in the caller
    /// (`MoodConfig::init_with`) — this only builds.
    pub async fn build_async(
        pool: &sqlx::SqlitePool,
        embedder: &Embedder,
        settings: &ColorAxesSettings,
        pairs: &[MoodEndpoint],
    ) -> Result<Self> {
        assert!(!pairs.is_empty());

        let v_base =
            crate::embed::get_or_embed_cached(pool, embedder, &settings.base_string, "").await?;

        let mut basis_moods = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let s = crate::embed::get_or_embed_cached(
                pool,
                embedder,
                &pair.mood,
                &settings.prefix_string,
            )
            .await?;
            let diff: Vec<f32> = s.iter().zip(&v_base).map(|(x, y)| x - y).collect();
            let norm_vector = crate::embed::normalize(&diff);
            let oklab = rgb_to_oklab(pair.color);
            basis_moods.push(BasisMood {
                mood: pair.mood.clone(),
                oklab,
                vector: norm_vector,
            });
        }

        let n = basis_moods.len();
        let mut gram_matrix = vec![vec![0.0_f32; n]; n];
        for i in 0..n {
            for j in 0..n {
                gram_matrix[i][j] =
                    crate::embed::dot(&basis_moods[i].vector, &basis_moods[j].vector);
            }
        }

        Ok(Self {
            basis_moods,
            base_vector: v_base,
            steepness: settings.blend_steepness.max(1.0),
            min_contribution: settings.min_contribution,
            top_k: settings.top_k,
            baseline_oklab_l: settings.baseline_oklab_l,
            emotional_saliency_gate: settings.effective_saliency_gate,
            prefix_string: settings.prefix_string.clone(),
            base_string: settings.base_string.clone(),
            gram_matrix,
        })
    }

    /// Run the NNLS regression, weight-rescaling, and saliency calculation
    /// stages of the pipeline for `embedding`, returning the contributing
    /// basis moods with their raw NNLS weights, rescaled weights, and the
    /// predicted emotional saliency.
    ///
    /// Returns `None` when the pipeline falls through to the neutral color:
    /// no basis moods, a zero-length target vector, a zero total NNLS weight,
    /// or no basis mood surviving the `min_contribution` filter.
    pub fn regression_weights(
        &self,
        embedding: &[f32],
        embedder: &Embedder,
        saliency: Result<f32, &str>,
    ) -> Option<MoodWeights> {
        let n = self.basis_moods.len();
        if n == 0 || embedding.len() != self.base_vector.len() {
            return None;
        }

        // 1. Compute shift vector length relative to base embedding without heap allocation
        let mut len_x_sq = 0.0_f32;
        for (&x, &b) in embedding.iter().zip(&self.base_vector) {
            let diff = x - b;
            len_x_sq += diff * diff;
        }
        let len_x_norm = len_x_sq.sqrt();
        if len_x_norm < 1e-6 {
            return None;
        }
        let inv_norm = 1.0 / len_x_norm;

        // 2. Compute at_b = A^T * target_vec directly without vector allocation
        let mut at_b = vec![0.0_f32; n];
        for (i, bm) in self.basis_moods.iter().enumerate() {
            let mut dot_sum = 0.0_f32;
            for ((&x, &b), &v) in embedding.iter().zip(&self.base_vector).zip(&bm.vector) {
                dot_sum += (x - b) * v;
            }
            at_b[i] = dot_sum * inv_norm;
        }

        // 3. Run NNLS on precomputed Gram matrix and at_b
        let weights = nnls_core(&self.gram_matrix, &at_b, 300);

        let total_weight: f32 = weights.iter().sum();
        if total_weight < 1e-6 {
            return None;
        }

        // 4. Filter out weights by contribution % < min_contribution
        let min_contrib_thresh = self.min_contribution.to_float();
        let mut raw: Vec<(usize, f32)> = weights
            .iter()
            .enumerate()
            .filter_map(|(i, &w)| {
                if w / total_weight >= min_contrib_thresh {
                    Some((i, w))
                } else {
                    None
                }
            })
            .collect();

        if raw.is_empty() {
            return None;
        }

        // Sort descending by weight and keep top_k
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if self.top_k > 0 && raw.len() > self.top_k {
            raw.truncate(self.top_k);
        }

        // 5. Compute rescaled weights using blend_steepness
        let max_w = raw.iter().fold(0.0_f32, |acc, (_, w)| acc.max(*w));

        let rescaled: Vec<f32> = if max_w < 1e-6 {
            vec![1.0 / raw.len() as f32; raw.len()]
        } else {
            let inv_max_w = 1.0 / max_w;
            let mut sum_u = 0.0_f32;
            let unnorm: Vec<f32> = raw
                .iter()
                .map(|(_, w)| {
                    let u = (w * inv_max_w).powf(self.steepness);
                    sum_u += u;
                    u
                })
                .collect();
            if sum_u > 0.0 {
                let inv_sum = 1.0 / sum_u;
                unnorm.into_iter().map(|u| u * inv_sum).collect()
            } else {
                vec![1.0 / raw.len() as f32; raw.len()]
            }
        };

        // 6. Emotional saliency: a caller-supplied override (`Ok(score)`) skips
        // the prediction; otherwise predict from the un-prefixed raw text
        // (`Err(mood_text)`, see [`predict_saliency`]).
        let saliency = match saliency {
            Ok(s) => s,
            Err(mood_text) => predict_saliency(embedder, mood_text),
        };

        Some(MoodWeights {
            raw,
            rescaled,
            saliency,
        })
    }

    /// Compute the final Oklab color from a [`MoodWeights`] regression
    /// result (produced by [`Self::regression_weights`]); `None` (the
    /// pipeline fell through) maps to the neutral baseline color.
    pub fn weights_to_color(&self, reg: Option<&MoodWeights>) -> Oklab {
        let l_neutral = self.baseline_oklab_l.to_float();
        let Some(reg) = reg else {
            return Oklab {
                l: l_neutral,
                a: 0.0,
                b: 0.0,
            };
        };

        // 5. Blend Oklab colors using rescaled weights
        let mut blended_l = 0.0;
        let mut blended_a = 0.0;
        let mut blended_b = 0.0;

        for ((idx, _), rw) in reg.raw.iter().zip(&reg.rescaled) {
            let color = self.basis_moods[*idx].oklab;
            blended_l += color.l * rw;
            blended_a += color.a * rw;
            blended_b += color.b * rw;
        }

        // 6. Saliency S is already computed for `mood_text` in
        //    `regression_weights`.
        let saliency = reg.saliency;

        // 7. Gate saliency: Seff = 1 + P*(S - 1), linearly interpolating raw
        //    saliency toward 1.0 so P=100 keeps S unchanged and P=0 disables it.
        let s_eff = self.effective_saliency(saliency);

        // 8. Apply formula:
        // L_final = L_neutral + Seff * (L_blended - L_neutral)
        // a_final = Seff * a_blended
        // b_final = Seff * b_blended
        let l_final = l_neutral + s_eff * (blended_l - l_neutral);
        let a_final = s_eff * blended_a;
        let b_final = s_eff * blended_b;

        Oklab {
            l: l_final,
            a: a_final,
            b: b_final,
        }
    }

    /// Effective saliency after the emotional gate, `Seff = 1 + P*(S - 1)`,
    /// using this axes' configured `emotional_saliency_gate` P.
    pub fn effective_saliency(&self, saliency: f32) -> f32 {
        gated_saliency(saliency, self.emotional_saliency_gate.to_float())
    }

    /// Resolve a feeling row to its final Oklab color within a single render
    /// run, caching the color per mood so repeated moods run the pipeline
    /// once.
    ///
    /// Uses the row's persisted (prefix-anchored) embedding BLOB when
    /// available; rows without one (legacy) have the mood embedded on the
    /// fly (with `self.prefix_string`) and are backfilled via
    /// [`crate::sql::update_feeling_embedding`]. The cached saliency score
    /// (`feeling.score`) is passed as the regression override when present
    /// (skips the ONNX saliency pass); rows without one are backfilled via
    /// [`crate::sql::update_feeling_score`].
    ///
    ///
    ///
    /// Returns `None` for empty moods or when embedding fails.
    pub async fn mood_color_cached(
        &self,
        pool: &SqlitePool,
        embedder: &Embedder,
        feeling: &FeelingRow,
        cache: &mut HashMap<String, Oklab>,
    ) -> Option<Oklab> {
        let mood = &feeling.mood;
        if let Some(oklab) = cache.get(mood) {
            return Some(*oklab);
        }
        if mood.is_empty() {
            return None;
        }
        let embedding = match feeling
            .embedding
            .as_deref()
            .and_then(crate::embed::blob_to_embedding)
        {
            Some(emb) => emb,
            None => match embedder.embed(mood, &self.prefix_string) {
                Ok(emb) => {
                    let blob_bytes = crate::embed::embedding_to_blob(&emb);
                    let _ =
                        crate::sql::update_feeling_embedding(pool, feeling.id, &blob_bytes).await;
                    emb
                }
                Err(_) => return None,
            },
        };
        // The cached score (when present) skips the saliency ONNX pass;
        // when absent, backfill what the regression just computed
        // (log-and-continue, mirroring the embedding backfill above).
        let reg = self.regression_weights(&embedding, embedder, feeling.score.ok_or(mood.as_str()));
        if let Some(reg) = &reg {
            if feeling.score.is_none() {
                let _ = crate::sql::update_feeling_score(pool, feeling.id, reg.saliency).await;
            }
        }
        let oklab = self.weights_to_color(reg.as_ref());
        cache.insert(mood.to_string(), oklab);
        Some(oklab)
    }
}

/// Lawson-Hanson Non-Negative Least Squares (NNLS) solver using precomputed Gram matrix A^T A and A^T b.
pub fn nnls_core(at_a: &[Vec<f32>], at_b: &[f32], max_iter: usize) -> Vec<f32> {
    let n = at_b.len();
    if n == 0 {
        return Vec::new();
    }

    let mut x = vec![0.0_f32; n];
    let mut passive = vec![false; n];
    let mut w = at_b.to_vec();

    let mut pass_indices = Vec::with_capacity(n);
    let mut z = vec![0.0_f32; n];
    let mut aug = Vec::new();

    for _ in 0..max_iter {
        let mut max_w = 0.0_f32;
        let mut max_idx = None;
        for i in 0..n {
            if !passive[i] && w[i] > max_w {
                max_w = w[i];
                max_idx = Some(i);
            }
        }

        let j = match max_idx {
            Some(idx) if max_w > 1e-6 => idx,
            _ => break,
        };

        passive[j] = true;

        loop {
            pass_indices.clear();
            for i in 0..n {
                if passive[i] {
                    pass_indices.push(i);
                }
            }

            let k = pass_indices.len();
            if k == 0 {
                break;
            }

            solve_sub_system_in_place(at_a, at_b, &pass_indices, &mut aug, &mut z);

            let mut all_pos = true;
            for &pi in &pass_indices {
                if z[pi] <= 1e-7 {
                    all_pos = false;
                    break;
                }
            }

            if all_pos {
                x.copy_from_slice(&z);
                break;
            }

            let mut alpha = f32::INFINITY;
            for &pi in &pass_indices {
                if z[pi] <= 1e-7 {
                    let denom = x[pi] - z[pi];
                    if denom.abs() > 1e-9 {
                        let a_val = x[pi] / denom;
                        if a_val < alpha {
                            alpha = a_val;
                        }
                    }
                }
            }

            if alpha.is_infinite() || alpha < 0.0 {
                alpha = 0.0;
            }

            for i in 0..n {
                x[i] += alpha * (z[i] - x[i]);
            }

            for &pi in &pass_indices {
                if x[pi].abs() <= 1e-6 {
                    x[pi] = 0.0;
                    passive[pi] = false;
                }
            }
        }

        for i in 0..n {
            let mut ax_i = 0.0_f32;
            for j in 0..n {
                ax_i += at_a[i][j] * x[j];
            }
            w[i] = at_b[i] - ax_i;
        }
    }

    x
}

/// Lawson-Hanson Non-Negative Least Squares (NNLS) solver.
/// Solves min || A x - b ||_2 s.t. x >= 0.
pub fn nnls(columns: &[Vec<f32>], b: &[f32], max_iter: usize) -> Vec<f32> {
    let n = columns.len();
    if n == 0 {
        return Vec::new();
    }

    let at_b: Vec<f32> = columns
        .iter()
        .map(|col| crate::embed::dot(col, b))
        .collect();

    let mut at_a = vec![vec![0.0_f32; n]; n];
    for i in 0..n {
        for j in 0..n {
            at_a[i][j] = crate::embed::dot(&columns[i], &columns[j]);
        }
    }

    nnls_core(&at_a, &at_b, max_iter)
}

/// In-place solver for sub-system A^T A * z = A^T b on passive index set using flat augmented matrix.
fn solve_sub_system_in_place(
    at_a: &[Vec<f32>],
    at_b: &[f32],
    pass_indices: &[usize],
    aug: &mut Vec<f32>,
    z: &mut [f32],
) {
    let k = pass_indices.len();
    let stride = k + 1;
    aug.clear();
    aug.resize(k * stride, 0.0);

    for (r, &pi) in pass_indices.iter().enumerate() {
        for (c, &pj) in pass_indices.iter().enumerate() {
            aug[r * stride + c] = at_a[pi][pj];
        }
        aug[r * stride + k] = at_b[pi];
    }

    for i in 0..k {
        let mut max_row = i;
        let mut max_val = aug[i * stride + i].abs();
        for r in (i + 1)..k {
            let val = aug[r * stride + i].abs();
            if val > max_val {
                max_val = val;
                max_row = r;
            }
        }

        if max_row != i {
            for c in i..=k {
                aug.swap(i * stride + c, max_row * stride + c);
            }
        }

        let pivot = aug[i * stride + i];
        if pivot.abs() < 1e-9 {
            continue;
        }

        let inv_pivot = 1.0 / pivot;
        for c in i..=k {
            aug[i * stride + c] *= inv_pivot;
        }

        for r in 0..k {
            if r != i {
                let factor = aug[r * stride + i];
                for c in i..=k {
                    aug[r * stride + c] -= factor * aug[i * stride + c];
                }
            }
        }
    }

    z.fill(0.0);
    for (r, &pi) in pass_indices.iter().enumerate() {
        z[pi] = aug[r * stride + k];
    }
}

/// Effective saliency after the emotional gate: `Seff = 1 + P*(S - 1)` for gate
/// fraction P in [0, 1]. P = 1.0 leaves raw saliency untouched; P = 0.0 disables
/// saliency (Seff = 1.0).
fn gated_saliency(saliency: f32, gate: f32) -> f32 {
    1.0 + gate * (saliency - 1.0)
}

/// Linear interpolation between two Oklab colors.
pub fn lerp_oklab(start: Oklab, end: Oklab, t: f32) -> Oklab {
    Oklab {
        l: start.l + t * (end.l - start.l),
        a: start.a + t * (end.a - start.a),
        b: start.b + t * (end.b - start.b),
    }
}

/// Average a list of Oklab colors component-wise; `None` when empty.
pub fn average_oklab(colors: &[Oklab]) -> Option<Oklab> {
    if colors.is_empty() {
        return None;
    }
    let mut sum = Oklab {
        l: 0.0,
        a: 0.0,
        b: 0.0,
    };
    for c in colors {
        sum.l += c.l;
        sum.a += c.a;
        sum.b += c.b;
    }
    let inv_n = 1.0 / colors.len() as f32;
    Some(Oklab {
        l: sum.l * inv_n,
        a: sum.a * inv_n,
        b: sum.b * inv_n,
    })
}

/// Helper function for tests.
pub fn blend_weights(normalized_scores: &[f32], steepness: f32) -> Vec<f32> {
    if normalized_scores.is_empty() {
        return Vec::new();
    }
    let max_delta = normalized_scores
        .iter()
        .map(|&t| (t - 0.5).abs())
        .fold(0.0_f32, f32::max);

    let mut weights: Vec<f32> = if max_delta <= 1e-6 {
        vec![1.0 / normalized_scores.len() as f32; normalized_scores.len()]
    } else {
        let inv_max_delta = 1.0 / max_delta;
        normalized_scores
            .iter()
            .map(|&t| (((t - 0.5).abs()) * inv_max_delta).powf(steepness))
            .collect()
    };

    let total_weight: f32 = weights.iter().sum();
    if total_weight > 0.0 {
        let inv_total = 1.0 / total_weight;
        for w in weights.iter_mut() {
            *w *= inv_total;
        }
    }
    weights
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nnls_simple() {
        // Solves A x = b where A is identity [1,0], [0,1] and b = [0.5, 0.8]
        let columns = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let b = vec![0.5, 0.8];
        let x = nnls(&columns, &b, 100);
        assert_eq!(x.len(), 2);
        assert!((x[0] - 0.5).abs() < 1e-4);
        assert!((x[1] - 0.8).abs() < 1e-4);
    }

    #[test]
    fn test_gated_saliency() {
        // P = 1.0: raw saliency preserved.
        assert_eq!(gated_saliency(0.5, 1.0), 0.5);
        assert_eq!(gated_saliency(1.0, 1.0), 1.0);
        // P = 0.0: saliency disabled -> always 1.0.
        assert_eq!(gated_saliency(0.3, 0.0), 1.0);
        // P = 0.8 (default): Seff = 1 + 0.8*(S - 1).
        assert!((gated_saliency(0.5, 0.8) - 0.6).abs() < 1e-6);
        assert!((gated_saliency(0.0, 0.8) - 0.2).abs() < 1e-6);
        assert!((gated_saliency(1.0, 0.8) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_nnls_nonnegative_constraint() {
        // Target is negative -> NNLS clamps x to 0
        let columns = vec![vec![1.0, 0.0]];
        let b = vec![-0.5, 0.0];
        let x = nnls(&columns, &b, 100);
        assert_eq!(x.len(), 1);
        assert_eq!(x[0], 0.0);
    }
}
