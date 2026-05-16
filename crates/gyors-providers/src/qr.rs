//! QR code generator. `qr <text>` produces a PNG image on clipboard
//!
//! Uses the `qrcode` crate to build bit matrix (without enabling its
//! optional `image` feature so we're not locked to its image version), then
//! paints the matrix into a grayscale `ImageBuffer` that's encoded as PNG

use async_trait::async_trait;
use base64::prelude::*;
use image::{ImageBuffer, Luma};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct QrProvider;

const MAX_TITLE_LEN: usize = 60;
/// Pixels per QR module
const MODULE_SCALE: u32 = 10;
/// Quiet-zone padding in pixels around the matrix
const QUIET_ZONE_PX: u32 = 40;

#[async_trait]
impl Provider for QrProvider {
    fn id(&self) -> &str {
        "qr"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(rest) = query.pattern().strip_prefix("qr ") else {
            return vec![];
        };
        let text = rest.trim();
        if text.is_empty() {
            return vec![];
        }
        vec![Candidate {
            id: format!("qr::{text}"),
            title: format!("QR: {}", truncate(text, MAX_TITLE_LEN)),
            subtitle: Some("↵ copy · → show inline".into()),
            icon: Icon::SfSymbol("qrcode".into()),
            kind: CandidateKind::Action,
            // Enter copies the PNG (the most common "I want to paste
            // this into Slack" move). -> shows the QR inline instantly
            // - ViewModel recognises the `preview` action id and skips
            // actions list
            actions: vec![
                Action::primary("Copy QR image"),
                Action::new("preview", "Show QR"),
            ],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        let text = id
            .strip_prefix("qr::")
            .ok_or_else(|| anyhow::anyhow!("invalid qr candidate id: {id}"))?;
        let png = generate_qr_png(text)?;
        let b64 = BASE64_STANDARD.encode(&png);
        match action {
            "default" => Ok(Effect::CopyImagePng(b64)),
            "preview" => Ok(Effect::ShowImagePng(b64)),
            other => anyhow::bail!("unknown action for qr: {other}"),
        }
    }
}

pub fn generate_qr_png(text: &str) -> anyhow::Result<Vec<u8>> {
    let code = qrcode::QrCode::new(text.as_bytes())?;
    let width = code.width();
    let colors = code.to_colors(); // Vec<qrcode::Color>, length width*width

    let img_size = (width as u32) * MODULE_SCALE + QUIET_ZONE_PX * 2;
    // Start with white background
    let mut img: ImageBuffer<Luma<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(img_size, img_size, Luma([255u8]));

    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                let x0 = QUIET_ZONE_PX + (x as u32) * MODULE_SCALE;
                let y0 = QUIET_ZONE_PX + (y as u32) * MODULE_SCALE;
                for py in 0..MODULE_SCALE {
                    for px in 0..MODULE_SCALE {
                        img.put_pixel(x0 + px, y0 + py, Luma([0u8]));
                    }
                }
            }
        }
    }

    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;
    Ok(buf)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.into()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = QrProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn empty_text_no_match() {
        let p = QrProvider;
        assert!(p.query(&Query::new("qr ")).await.is_empty());
        assert!(p.query(&Query::new("qr     ")).await.is_empty());
    }

    #[tokio::test]
    async fn qr_produces_candidate() {
        let p = QrProvider;
        let out = p.query(&Query::new("qr https://example.com")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.starts_with("QR:"));
        assert!(out[0].bypass_rank);
    }

    #[tokio::test]
    async fn activate_default_copies_the_png() {
        // Enter on a QR candidate copies the PNG - the common case
        let p = QrProvider;
        let out = p.query(&Query::new("qr hello")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyImagePng(b64) => {
                let bytes = BASE64_STANDARD.decode(&b64).expect("valid base64");
                assert_eq!(&bytes[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
            }
            other => panic!("expected CopyImagePng, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_preview_shows_inline() {
        // -> on a QR candidate jumps straight to the inline preview
        // (ViewModel recognises "preview" and skips actions list)
        let p = QrProvider;
        let out = p.query(&Query::new("qr hello")).await;
        let effect = p.activate(&out[0].id, "preview").await.unwrap();
        match effect {
            Effect::ShowImagePng(b64) => {
                let bytes = BASE64_STANDARD.decode(&b64).expect("valid base64");
                assert_eq!(&bytes[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
            }
            other => panic!("expected ShowImagePng, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_action_errors() {
        let p = QrProvider;
        let out = p.query(&Query::new("qr hello")).await;
        assert!(p.activate(&out[0].id, "bogus").await.is_err());
    }

    #[tokio::test]
    async fn qr_candidate_exposes_preview_and_copy_actions() {
        let p = QrProvider;
        let out = p.query(&Query::new("qr hi")).await;
        assert!(out[0].actions.iter().any(|a| a.id == "default"));
        assert!(out[0].actions.iter().any(|a| a.id == "preview"));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = QrProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn generate_qr_png_returns_valid_png() {
        let bytes = generate_qr_png("hello world").unwrap();
        assert!(bytes.len() > 100);
        assert_eq!(&bytes[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    }

    #[test]
    fn generate_qr_png_handles_long_input() {
        let long_text: String = "x".repeat(500);
        let bytes = generate_qr_png(&long_text).unwrap();
        assert!(bytes.len() > 100);
    }
}
