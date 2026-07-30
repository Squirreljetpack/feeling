//! Mood color computation via NNLS basis-ray regression & saliency scaling.
//!
//! Mood strings are embedded with the bge-small model, projected onto a
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

use crate::color_conversion::rgb_to_oklab;
use crate::config::MoodConfig;
use crate::embed::Embedder;
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
    pub prefix: String,
}

/// NNLS regression output for one embedding: the contributing basis moods
/// with their raw NNLS weights and the rescaled weights used for blending.
#[derive(Debug)]
pub struct RegressionWeights {
    /// (basis mood index, raw NNLS weight), filtered by `min_contribution`,
    /// sorted descending, truncated to `top_k` — same order as [`Self::rescaled`].
    pub raw: Vec<(usize, f32)>,
    /// Power-weighted rescale of `raw`, normalized to sum 1.
    pub rescaled: Vec<f32>,
}

impl ColorAxes {
    /// Build basis vectors from config endpoint pairs using SQLite cached embeddings.
    pub async fn build_async(
        pool: &sqlx::SqlitePool,
        embedder: &Embedder,
        config: &MoodConfig,
    ) -> Result<Self> {
        if let Some(axes) = &config.color_axes {
            return Ok(axes.clone());
        }
        if config.pairs.is_empty() {
            anyhow::bail!("moods.pairs must contain at least one mood pair");
        }

        let v_base =
            crate::embed::get_or_embed_cached(pool, embedder, &config.neutral_string, "").await?;

        let mut basis_moods = Vec::with_capacity(config.pairs.len());
        for pair in &config.pairs {
            let s = crate::embed::get_or_embed_cached(pool, embedder, &pair.mood, &config.prefix)
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

        Ok(Self {
            basis_moods,
            base_vector: v_base,
            steepness: config.blend_steepness.max(1.0),
            min_contribution: config.min_contribution,
            top_k: config.top_k,
            baseline_oklab_l: config.baseline_oklab_l,
            emotional_saliency_gate: config.effective_saliency_gate,
            prefix: config.prefix.clone(),
        })
    }

    /// Run the NNLS regression and weight-rescaling stages of the pipeline
    /// for `embedding`, returning the contributing basis moods with their raw
    /// NNLS weights and the rescaled (power-weighted, normalized) weights.
    ///
    /// Returns `None` when the pipeline falls through to the neutral color:
    /// no basis moods, a zero-length target vector, a zero total NNLS weight,
    /// or no basis mood surviving the `min_contribution` filter.
    pub fn regression_weights(&self, embedding: &[f32]) -> Option<RegressionWeights> {
        if self.basis_moods.is_empty() {
            return None;
        }

        // 1. Shift vector relative to base embedding
        let v_x: Vec<f32> = embedding
            .iter()
            .zip(&self.base_vector)
            .map(|(x, b)| x - b)
            .collect();
        let len_x_norm: f32 = v_x.iter().map(|v| v * v).sum::<f32>().sqrt();

        if len_x_norm < 1e-6 {
            return None;
        }

        let target_vec = crate::embed::normalize(&v_x);

        // 2. Run NNLS on normalized basis vectors
        let columns: Vec<Vec<f32>> = self
            .basis_moods
            .iter()
            .map(|bm| bm.vector.clone())
            .collect();
        let weights = nnls(&columns, &target_vec, 300);

        let total_weight: f32 = weights.iter().sum();
        if total_weight < 1e-6 {
            return None;
        }

        // 3. Filter out weights by contribution % < min_contribution
        let mut raw: Vec<(usize, f32)> = weights
            .iter()
            .enumerate()
            .map(|(i, &w)| (i, w))
            .filter(|&(_, w)| w / total_weight >= self.min_contribution.to_float())
            .collect();

        if raw.is_empty() {
            return None;
        }

        // Sort descending by weight and keep top_k
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if self.top_k > 0 && raw.len() > self.top_k {
            raw.truncate(self.top_k);
        }

        // 4. Compute rescaled weights using blend_steepness
        let max_w = raw.iter().map(|(_, w)| *w).fold(0.0_f32, f32::max);

        let rescaled: Vec<f32> = if max_w < 1e-6 {
            vec![1.0 / raw.len() as f32; raw.len()]
        } else {
            let unnorm: Vec<f32> = raw
                .iter()
                .map(|(_, w)| (w / max_w).powf(self.steepness))
                .collect();
            let sum_u: f32 = unnorm.iter().sum();
            if sum_u > 0.0 {
                unnorm.iter().map(|u| u / sum_u).collect()
            } else {
                vec![1.0 / raw.len() as f32; raw.len()]
            }
        };

        Some(RegressionWeights { raw, rescaled })
    }

    /// Compute the final Oklab color for an embedding, optionally using raw text & embedder for saliency prediction.
    pub fn project_full(
        &self,
        embedding: &[f32],
        embedder: Option<&Embedder>,
        mood_text: Option<&str>,
    ) -> Oklab {
        let l_neutral = self.baseline_oklab_l.to_float();
        let Some(reg) = self.regression_weights(embedding) else {
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

        // 6. Compute saliency S from raw embedding using saliency adaptor
        let saliency = match (embedder, mood_text) {
            (Some(emb), Some(text)) if !text.trim().is_empty() => {
                if let Ok(raw_emb) = emb.embed(text, "") {
                    emb.predict_saliency(&raw_emb)
                } else {
                    1.0
                }
            }
            _ => 1.0,
        };

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

    /// Project embedding with default saliency (1.0) if no embedder/text is passed.
    pub fn project(&self, embedding: &[f32]) -> Oklab {
        self.project_full(embedding, None, None)
    }

    /// Compute the final Oklab color for a mood string from its
    /// (already computed) prefix-anchored embedding, caching the color per
    /// mood so repeated moods within one render run the pipeline once.
    ///
    /// The caller is responsible for embedding `mood` (with `self.prefix`)
    /// and persisting it, e.g. backfilling a feeling row.
    pub fn mood_color(
        &self,
        embedder: &Embedder,
        embedding: &[f32],
        mood: &str,
        cache: &mut HashMap<String, Oklab>,
    ) -> Option<Oklab> {
        if let Some(oklab) = cache.get(mood) {
            return Some(*oklab);
        }
        let oklab = self.project_full(embedding, Some(embedder), Some(mood));
        cache.insert(mood.to_string(), oklab);
        Some(oklab)
    }
}

/// Lawson-Hanson Non-Negative Least Squares (NNLS) solver.
/// Solves min || A x - b ||_2 s.t. x >= 0.
pub fn nnls(columns: &[Vec<f32>], b: &[f32], max_iter: usize) -> Vec<f32> {
    let n = columns.len();
    if n == 0 {
        return Vec::new();
    }

    let mut x = vec![0.0_f32; n];
    let mut passive = vec![false; n];

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

    let mut w = at_b.clone();

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
            let pass_indices: Vec<usize> = (0..n).filter(|&i| passive[i]).collect();
            let k = pass_indices.len();
            if k == 0 {
                break;
            }

            let mut sub_a = vec![vec![0.0_f32; k]; k];
            let mut sub_b = vec![0.0_f32; k];
            for (r, &pi) in pass_indices.iter().enumerate() {
                sub_b[r] = at_b[pi];
                for (c, &pj) in pass_indices.iter().enumerate() {
                    sub_a[r][c] = at_a[pi][pj];
                }
            }

            let z_sub = solve_linear_system(&sub_a, &sub_b);
            let mut z = vec![0.0_f32; n];
            for (r, &pi) in pass_indices.iter().enumerate() {
                z[pi] = z_sub[r];
            }

            let mut all_pos = true;
            for &pi in &pass_indices {
                if z[pi] <= 1e-7 {
                    all_pos = false;
                    break;
                }
            }

            if all_pos {
                x = z;
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

/// Helper: Solve A x = b for a small (k x k) system via Gaussian elimination with partial pivoting.
fn solve_linear_system(a: &[Vec<f32>], b: &[f32]) -> Vec<f32> {
    let k = b.len();
    if k == 0 {
        return Vec::new();
    }
    let mut aug: Vec<Vec<f32>> = (0..k)
        .map(|r| {
            let mut row = a[r].clone();
            row.push(b[r]);
            row
        })
        .collect();

    for i in 0..k {
        let mut max_row = i;
        for r in (i + 1)..k {
            if aug[r][i].abs() > aug[max_row][i].abs() {
                max_row = r;
            }
        }
        aug.swap(i, max_row);

        let pivot = aug[i][i];
        if pivot.abs() < 1e-9 {
            continue;
        }

        for c in i..=k {
            aug[i][c] /= pivot;
        }

        for r in 0..k {
            if r != i {
                let factor = aug[r][i];
                for c in i..=k {
                    aug[r][c] -= factor * aug[i][c];
                }
            }
        }
    }

    (0..k).map(|r| aug[r][k]).collect()
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
    let n = colors.len() as f32;
    Some(Oklab {
        l: sum.l / n,
        a: sum.a / n,
        b: sum.b / n,
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
        normalized_scores
            .iter()
            .map(|&t| ((t - 0.5).abs() / max_delta).powf(steepness))
            .collect()
    };

    let total_weight: f32 = weights.iter().sum();
    for w in weights.iter_mut() {
        *w /= total_weight;
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
