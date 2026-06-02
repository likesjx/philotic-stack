use crate::hub::VisionHandle;
use anyhow::{Context, Result};
use ndarray::{Array2, Array3, Array4};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

// ── Florence-2 image normalisation constants ─────────────────────────────────
const IMAGE_SIZE: u32 = 768;
const IMAGE_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGE_STD: [f32; 3] = [0.229, 0.224, 0.225];

// Florence-2 uses Bart-style tokeniser: BOS=<s> EOS=</s>
const BART_BOS_ID: i64 = 0;
const BART_EOS_ID: i64 = 2;

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VisionConfig {
    /// HuggingFace repo id, e.g. `"onnx-community/Florence-2-base-ft"`.
    pub repo_id: String,
    /// Maximum tokens to generate per inference call.
    pub max_new_tokens: usize,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            repo_id: "onnx-community/Florence-2-base-ft".into(),
            max_new_tokens: 1024,
        }
    }
}

// ── Output types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OcrOutput {
    pub text: String,
    pub model_gen: String,
}

/// A bounding box in normalised [0, 1] coordinates.
#[derive(Debug, Clone)]
pub struct BoundingBox {
    pub label: String,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

#[derive(Debug, Clone)]
pub struct GroundOutput {
    pub results: Vec<BoundingBox>,
    pub model_gen: String,
}

// ── Backend ───────────────────────────────────────────────────────────────────

/// Local ONNX Florence-2 vision backend.
///
/// Runs the Florence-2 merged encoder once per image+prompt, then greedily
/// decodes with the non-merged decoder.  No KV-cache; O(n²) decode — correct
/// for the proof path.
pub struct VisionBackend {
    encoder: Arc<Mutex<Session>>,
    decoder: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
    model_gen: String,
    max_new_tokens: usize,
}

impl VisionBackend {
    pub fn load(handle: &VisionHandle, config: &VisionConfig) -> Result<Self> {
        let encoder = Session::builder()
            .map_err(|e| anyhow::anyhow!("encoder builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("encoder opt level: {e}"))?
            .commit_from_file(&handle.encoder_path)
            .with_context(|| {
                format!(
                    "load Florence-2 encoder from {}",
                    handle.encoder_path.display()
                )
            })?;

        let decoder = Session::builder()
            .map_err(|e| anyhow::anyhow!("decoder builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("decoder opt level: {e}"))?
            .commit_from_file(&handle.decoder_path)
            .with_context(|| {
                format!(
                    "load Florence-2 decoder from {}",
                    handle.decoder_path.display()
                )
            })?;

        let tokenizer = Tokenizer::from_file(&handle.tokenizer_path)
            .map_err(|e| anyhow::anyhow!("load Florence-2 tokenizer: {e}"))?;

        tracing::info!(
            model_gen = %handle.model_gen,
            encoder = %handle.encoder_path.display(),
            decoder = %handle.decoder_path.display(),
            "VisionBackend loaded (Florence-2)"
        );

        Ok(Self {
            encoder: Arc::new(Mutex::new(encoder)),
            decoder: Arc::new(Mutex::new(decoder)),
            tokenizer: Arc::new(tokenizer),
            model_gen: handle.model_gen.clone(),
            max_new_tokens: config.max_new_tokens,
        })
    }

    pub fn model_gen(&self) -> &str {
        &self.model_gen
    }

    // ── Image preprocessing ───────────────────────────────────────────────────

    fn preprocess_image(&self, image_bytes: &[u8]) -> Result<Array4<f32>> {
        use image::imageops::FilterType;

        let img = image::load_from_memory(image_bytes).context("decode image bytes")?;
        let rgb = img
            .resize_exact(IMAGE_SIZE, IMAGE_SIZE, FilterType::Lanczos3)
            .to_rgb8();

        let h = IMAGE_SIZE as usize;
        let w = IMAGE_SIZE as usize;
        let mut tensor = Array4::<f32>::zeros([1, 3, h, w]);

        for (x, y, pixel) in rgb.enumerate_pixels() {
            let r = pixel[0] as f32 / 255.0;
            let g = pixel[1] as f32 / 255.0;
            let b = pixel[2] as f32 / 255.0;
            tensor[[0, 0, y as usize, x as usize]] = (r - IMAGE_MEAN[0]) / IMAGE_STD[0];
            tensor[[0, 1, y as usize, x as usize]] = (g - IMAGE_MEAN[1]) / IMAGE_STD[1];
            tensor[[0, 2, y as usize, x as usize]] = (b - IMAGE_MEAN[2]) / IMAGE_STD[2];
        }

        Ok(tensor)
    }

    // ── Encoder ───────────────────────────────────────────────────────────────

    fn run_encoder(
        &self,
        pixel_values: Array4<f32>,
        prompt: &str,
    ) -> Result<(Vec<f32>, usize, usize, Vec<i64>)> {
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("tokenize prompt: {e}"))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let seq_len = input_ids.len();

        let ids_arr =
            Array2::from_shape_vec([1, seq_len], input_ids.clone()).context("shape input_ids")?;
        let mask_arr = Array2::from_shape_vec([1, seq_len], attention_mask.clone())
            .context("shape attention_mask")?;

        let px_tensor = Tensor::<f32>::from_array(pixel_values)
            .map_err(|e| anyhow::anyhow!("pixel_values tensor: {e}"))?;
        let ids_tensor = Tensor::<i64>::from_array(ids_arr)
            .map_err(|e| anyhow::anyhow!("input_ids tensor: {e}"))?;
        let mask_tensor = Tensor::<i64>::from_array(mask_arr)
            .map_err(|e| anyhow::anyhow!("attention_mask tensor: {e}"))?;

        let (enc_data, enc_seq, enc_hidden) = {
            let mut enc = self
                .encoder
                .lock()
                .map_err(|_| anyhow::anyhow!("encoder mutex poisoned"))?;
            let enc_out = enc
                .run(ort::inputs![
                    "pixel_values"   => px_tensor,
                    "input_ids"      => ids_tensor,
                    "attention_mask" => mask_tensor
                ])
                .map_err(|e| anyhow::anyhow!("encoder run: {e}"))?;

            let (shape, data) = enc_out[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract encoder output: {e}"))?;
            let dims: &[i64] = &shape;
            anyhow::ensure!(
                dims.len() == 3,
                "expected 3D encoder output, got {}D",
                dims.len()
            );
            (data.to_vec(), dims[1] as usize, dims[2] as usize)
        };

        Ok((enc_data, enc_seq, enc_hidden, attention_mask))
    }

    // ── Greedy decoder ────────────────────────────────────────────────────────

    fn greedy_decode(
        &self,
        enc_data: &[f32],
        enc_seq: usize,
        enc_hidden: usize,
        enc_mask: &[i64],
    ) -> Result<Vec<i64>> {
        let mut token_ids: Vec<i64> = vec![BART_BOS_ID];

        for _ in 0..self.max_new_tokens {
            let seq_len = token_ids.len();

            let ids_arr = Array2::from_shape_vec([1, seq_len], token_ids.clone())
                .context("shape decoder input_ids")?;
            let ids_tensor = Tensor::<i64>::from_array(ids_arr)
                .map_err(|e| anyhow::anyhow!("decoder input_ids tensor: {e}"))?;

            let enc_arr = Array3::from_shape_vec([1, enc_seq, enc_hidden], enc_data.to_vec())
                .context("shape encoder_hidden_states")?;
            let enc_tensor = Tensor::<f32>::from_array(enc_arr)
                .map_err(|e| anyhow::anyhow!("encoder_hidden_states tensor: {e}"))?;

            let enc_mask_arr = Array2::from_shape_vec([1, enc_mask.len()], enc_mask.to_vec())
                .context("shape encoder_attention_mask")?;
            let enc_mask_tensor = Tensor::<i64>::from_array(enc_mask_arr)
                .map_err(|e| anyhow::anyhow!("encoder_attention_mask tensor: {e}"))?;

            let next_token = {
                let mut dec = self
                    .decoder
                    .lock()
                    .map_err(|_| anyhow::anyhow!("decoder mutex poisoned"))?;
                let dec_out = dec
                    .run(ort::inputs![
                        "input_ids"               => ids_tensor,
                        "encoder_hidden_states"   => enc_tensor,
                        "encoder_attention_mask"  => enc_mask_tensor
                    ])
                    .map_err(|e| anyhow::anyhow!("decoder run: {e}"))?;

                let (shape, data) = dec_out[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow::anyhow!("extract logits: {e}"))?;
                let dims: &[i64] = &shape;
                let vocab = dims[2] as usize;
                let offset = (seq_len - 1) * vocab;
                data[offset..offset + vocab]
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(i, _)| i as i64)
                    .unwrap_or(BART_EOS_ID)
            };

            if next_token == BART_EOS_ID {
                break;
            }
            token_ids.push(next_token);
        }

        // Strip the leading BOS token.
        Ok(token_ids[1..].to_vec())
    }

    fn decode_tokens(&self, token_ids: &[i64]) -> Result<String> {
        let u32_ids: Vec<u32> = token_ids.iter().map(|&id| id as u32).collect();
        self.tokenizer
            .decode(&u32_ids, true)
            .map_err(|e| anyhow::anyhow!("decode tokens: {e}"))
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Extract all text from the image using Florence-2's OCR task.
    pub fn ocr(&self, image_bytes: &[u8]) -> Result<OcrOutput> {
        let pixel_values = self.preprocess_image(image_bytes)?;
        let (enc_data, enc_seq, enc_hidden, enc_mask) = self.run_encoder(pixel_values, "<OCR>")?;
        let token_ids = self.greedy_decode(&enc_data, enc_seq, enc_hidden, &enc_mask)?;
        let text = self.decode_tokens(&token_ids)?;

        Ok(OcrOutput {
            text,
            model_gen: self.model_gen.clone(),
        })
    }

    /// Locate objects matching `query` in the image.  Returns bounding boxes in
    /// normalised [0, 1] coordinates parsed from Florence-2's `<loc_NNN>` tokens.
    pub fn ground(&self, image_bytes: &[u8], query: &str) -> Result<GroundOutput> {
        let pixel_values = self.preprocess_image(image_bytes)?;
        let prompt = format!(
            "<REFERRING_EXPRESSION_SEGMENTATION>{}</REFERRING_EXPRESSION_SEGMENTATION>",
            query
        );
        let (enc_data, enc_seq, enc_hidden, enc_mask) = self.run_encoder(pixel_values, &prompt)?;
        let token_ids = self.greedy_decode(&enc_data, enc_seq, enc_hidden, &enc_mask)?;
        let decoded = self.decode_tokens(&token_ids)?;
        let results = parse_loc_tokens(&decoded, query);

        Ok(GroundOutput {
            results,
            model_gen: self.model_gen.clone(),
        })
    }
}

// ── Location token parser ─────────────────────────────────────────────────────

/// Parse Florence-2 `<loc_NNN>` tokens from decoded output.
/// Each group of four values encodes (x1, y1, x2, y2) in [0, 999] → [0, 1].
fn parse_loc_tokens(text: &str, query: &str) -> Vec<BoundingBox> {
    let mut loc_values: Vec<f32> = Vec::new();
    let mut search = text;

    while let Some(start) = search.find("<loc_") {
        search = &search[start + 5..]; // skip "<loc_"
        if let Some(end) = search.find('>') {
            if let Ok(n) = search[..end].parse::<f32>() {
                loc_values.push(n / 999.0);
            }
            search = &search[end + 1..];
        } else {
            break;
        }
    }

    loc_values
        .chunks(4)
        .filter(|c| c.len() == 4)
        .map(|c| BoundingBox {
            label: query.to_string(),
            x1: c[0],
            y1: c[1],
            x2: c[2],
            y2: c[3],
        })
        .collect()
}
