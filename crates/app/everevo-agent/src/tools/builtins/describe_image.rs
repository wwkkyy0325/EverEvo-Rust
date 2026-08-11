//! `describe_image` — vision-capable image description tool.
//!
//! Primary path: sends the image to a **dedicated vision LLM** (e.g. qwen3-vl-2b
//! served by llama.cpp) as an OpenAI multimodal message. The vision model is a
//! separate `[[llm]]` entry selected via `[routing] visionModelId` — distinct
//! from the plain-text main model, which never sees raw pixels.
//!
//! Fallback: deterministic offline scripts (`chess_fen.py` / `fractions_ocr.py`)
//! for benchmark domains. The tool returns an informative pointer to those
//! scripts when no vision model is configured, when the vision call fails, or
//! when the image is unsupported/oversized — so the agent degrades gracefully.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use everevo_core::llm::{ImageData, LlmMessage, LlmProvider};
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Raw-file guard. Beyond this, the base64 payload risks blowing past the vision
/// provider's 32K-context cap (the 6GB VRAM setup).
const MAX_IMAGE_BYTES: u64 = 6 * 1024 * 1024;

pub struct DescribeImageTool {
    /// Dedicated vision LLM. None → informative fallback pointer to offline scripts.
    vision: Option<Arc<dyn LlmProvider>>,
    /// Directory holding offline tool scripts (data/bench/tooltest). Used only to
    /// name the fallback scripts in the output.
    tooltest_dir: Option<PathBuf>,
}

impl DescribeImageTool {
    pub fn new(vision: Option<Arc<dyn LlmProvider>>, tooltest_dir: Option<PathBuf>) -> Self {
        Self {
            vision,
            tooltest_dir,
        }
    }

    fn default_question() -> &'static str {
        "Describe this image in detail. Transcribe any visible text, numbers, \
         symbols, formulas, tables, chess boards, or diagrams exactly as shown."
    }

    fn mime_for_path(path: &Path) -> Option<&'static str> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            "bmp" => Some("image/bmp"),
            "tif" | "tiff" => Some("image/tiff"),
            _ => None,
        }
    }

    fn fallback_pointer(&self) -> String {
        let dir = self
            .tooltest_dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "data/bench/tooltest".into());
        format!(
            "Vision model unavailable. Use the offline deterministic scripts instead:\n\
             - Chess board image → `{dir}/chess_fen.py <image_path>` (returns best SAN move)\n\
             - Fraction OCR image → `{dir}/fractions_ocr.py <image_path>` (returns transcribed worksheet)\n\
             For other images, no offline fallback exists — ask the user for a text description."
        )
    }
}

#[async_trait]
impl Tool for DescribeImageTool {
    fn name(&self) -> &str {
        "describe_image"
    }

    fn description(&self) -> &str {
        "Describe or analyze an image at a given file path using a dedicated vision model. \
         Parameters: path (required — absolute path to the image file), \
         question (optional — a specific question about the image; defaults to a general \
         detailed description). If the vision model is unavailable it reports so — then \
         fall back to the offline scripts chess_fen.py (chess board → SAN best move) and \
         fractions_ocr.py (fraction worksheet → transcription)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the image file (png/jpg/gif/webp/bmp/tiff)"
                },
                "question": {
                    "type": "string",
                    "description": "Optional specific question about the image (defaults to general detailed description)"
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let path_str = params["path"].as_str().unwrap_or("");
        if path_str.is_empty() {
            return Ok(ToolOutput {
                content: "path is required".into(),
                is_error: true,
                ..Default::default()
            });
        }
        let path = PathBuf::from(path_str);

        let Some(vision) = &self.vision else {
            return Ok(ToolOutput::text(self.fallback_pointer()));
        };

        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("无法读取图片 {}: {e}", path.display()),
                    is_error: true,
                    ..Default::default()
                });
            }
        };
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Ok(ToolOutput {
                content: format!(
                    "图片 {} 超过 {}MB 上限，改用离线脚本或压缩图片。{}",
                    path.display(),
                    MAX_IMAGE_BYTES / 1024 / 1024,
                    self.fallback_pointer()
                ),
                is_error: true,
                ..Default::default()
            });
        }
        let Some(mime) = Self::mime_for_path(&path) else {
            return Ok(ToolOutput {
                content: format!(
                    "不支持的图片格式: {}（仅支持 png/jpg/gif/webp/bmp/tiff）。{}",
                    path.display(),
                    self.fallback_pointer()
                ),
                is_error: true,
                ..Default::default()
            });
        };

        let question = params["question"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| Self::default_question().to_string());
        let mut msg = LlmMessage::user(question);
        msg.images = vec![ImageData {
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime_type: mime.to_string(),
        }];

        match vision.chat(&[msg], &[]).await {
            Ok(resp) => match resp.content {
                Some(text) if !text.trim().is_empty() => {
                    Ok(ToolOutput::text(text.trim().to_string()))
                }
                _ => Ok(ToolOutput::text(format!(
                    "视觉模型返回空结果，改用离线脚本。{}",
                    self.fallback_pointer()
                ))),
            },
            Err(e) => Ok(ToolOutput {
                content: format!(
                    "视觉模型不可用（{e}），改用离线脚本。{}",
                    self.fallback_pointer()
                ),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::MockLlmProvider;

    /// Minimal valid 1×1 PNG (used to exercise the read + mime + base64 path).
    const MINI_PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, // signature
        0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // IHDR
        0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', 0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
        0x00, 0x00, 0x1B, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82, // IEND
    ];

    fn write_mini_png(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, MINI_PNG).unwrap();
        p
    }

    #[tokio::test]
    async fn sends_image_to_vision_model() {
        let mock = Arc::new(MockLlmProvider::new().with_text("A chess board"));
        let tool = DescribeImageTool::new(Some(mock.clone()), None);
        let dir = tempfile::tempdir().unwrap();
        let png = write_mini_png(dir.path(), "board.png");

        let out = tool
            .execute(
                serde_json::json!({ "path": png.display().to_string() }),
                None,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("A chess board"));

        // The message sent to the vision model must carry exactly one image.
        let log = mock.call_log();
        assert_eq!(log.len(), 1);
        let msg = &log[0][0];
        assert_eq!(msg.images.len(), 1);
        assert_eq!(msg.images[0].mime_type, "image/png");
        assert!(!msg.images[0].data.is_empty());
    }

    #[tokio::test]
    async fn no_vision_model_returns_fallback_pointer() {
        let tool = DescribeImageTool::new(None, Some(PathBuf::from("data/bench/tooltest")));
        let out = tool
            .execute(serde_json::json!({ "path": "x.png" }), None)
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("chess_fen.py"));
        assert!(out.content.contains("fractions_ocr.py"));
    }

    #[tokio::test]
    async fn vision_error_returns_error_and_fallback() {
        // Mock with zero responses → chat() returns Err("no more responses").
        let mock = Arc::new(MockLlmProvider::new());
        let tool = DescribeImageTool::new(Some(mock), None);
        let dir = tempfile::tempdir().unwrap();
        let png = write_mini_png(dir.path(), "board.png");

        let out = tool
            .execute(
                serde_json::json!({ "path": png.display().to_string() }),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("视觉模型不可用"));
        assert!(out.content.contains("chess_fen.py"));
    }

    #[tokio::test]
    async fn missing_path_is_error() {
        let tool = DescribeImageTool::new(None, None);
        let out = tool.execute(serde_json::json!({}), None).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("path is required"));
    }

    #[tokio::test]
    async fn unsupported_extension_rejected() {
        let mock = Arc::new(MockLlmProvider::new().with_text("never called"));
        let tool = DescribeImageTool::new(Some(mock), None);
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("note.txt");
        std::fs::write(&txt, "hello").unwrap();

        let out = tool
            .execute(
                serde_json::json!({ "path": txt.display().to_string() }),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("不支持的图片格式"));
    }
}
