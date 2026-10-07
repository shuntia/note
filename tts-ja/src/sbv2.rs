//! Style-Bert-VITS2 JP-Extra on onnxruntime, with sbv2_core's Japanese front end.

use std::path::Path;

use anyhow::Context;
use ndarray::Array1;
use ort::execution_providers::cuda::CuDNNConvAlgorithmSearch;
use ort::execution_providers::{ArenaExtendStrategy, CPUExecutionProvider, CUDAExecutionProvider};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use sbv2_core::jtalk::JTalk;
use sbv2_core::tokenizer::Tokenizer;

pub const SAMPLE_RATE: u32 = 44100;

// Style-Bert-VITS2's own defaults.
const SDP_RATIO: f32 = 0.2;
const NOISE: f32 = 0.6;
const NOISE_W: f32 = 0.8;
const LENGTH: f32 = 1.0;

pub struct Sbv2 {
    bert: Session,
    vits: Session,
    tokenizer: Tokenizer,
    jtalk: JTalk,
    style: Array1<f32>,
    pub on_gpu: bool,
}

impl Sbv2 {
    /// Loads `dir`'s deberta.onnx, tokenizer.json, model.onnx and style_vectors.json; on the GPU
    /// when `gpu`, failing rather than falling back to the CPU.
    pub fn load(dir: &Path, gpu: bool) -> anyhow::Result<Self> {
        let session = |file: &str| -> anyhow::Result<Session> {
            let provider = if gpu {
                // Every chunk has new shapes, and any cuDNN algorithm search reruns on each.
                CUDAExecutionProvider::default()
                    .with_conv_algorithm_search(CuDNNConvAlgorithmSearch::Default)
                    .with_conv_max_workspace(false)
                    .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
                    .build()
                    .error_on_failure()
            } else {
                CPUExecutionProvider::default().build()
            };
            Session::builder()?
                .with_execution_providers([provider])?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_intra_threads(num_cpus::get_physical().min(8))?
                .commit_from_file(dir.join(file))
                .with_context(|| format!("loading {file}"))
        };
        let styles = sbv2_core::style::load_style(std::fs::read(dir.join("style_vectors.json"))?)?;
        Ok(Sbv2 {
            bert: session("deberta.onnx")?,
            vits: session("model.onnx")?,
            tokenizer: sbv2_core::tokenizer::get_tokenizer(std::fs::read(
                dir.join("tokenizer.json"),
            )?)?,
            jtalk: JTalk::new()?,
            style: sbv2_core::style::get_style_vector(&styles, 0, 1.0)?,
            on_gpu: gpu,
        })
    }

    /// Mono samples at `SAMPLE_RATE`.
    pub fn render(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        let bert = &mut self.bert;
        let (bert_features, phones, tones, languages) = sbv2_core::tts_util::parse_text_blocking(
            text,
            None,
            &self.jtalk,
            &self.tokenizer,
            |ids, mask| sbv2_core::bert::predict(bert, ids, mask),
        )?;
        let audio = sbv2_core::model::synthesize(
            &mut self.vits,
            bert_features,
            blanks_between(phones),
            Array1::from_vec(vec![0]),
            blanks_between(tones),
            blanks_between(languages),
            self.style.clone(),
            SDP_RATIO,
            LENGTH,
            NOISE,
            NOISE_W,
        )?;
        Ok(audio.into_iter().collect())
    }
}

/// sbv2_core 0.2.0-alpha8 intersperses blanks as `[x0, 0, x1, 0, …, 0]`; the model was trained on
/// `[0, x0, 0, x1, …, 0]`, which the BERT alignment also assumes.
fn blanks_between(seq: Array1<i64>) -> Array1<i64> {
    std::iter::once(0)
        .chain(seq.iter().take(seq.len().saturating_sub(1)).copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanks_fall_between_the_symbols() {
        let shifted = sbv2_core::utils::intersperse(&[5i64, 6, 7], 0);
        assert_eq!(
            blanks_between(Array1::from_vec(shifted)).to_vec(),
            [0, 5, 0, 6, 0, 7, 0]
        );
    }
}
