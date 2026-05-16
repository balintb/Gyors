//! MIME-type reference. `mime pdf` -> application/pdf - also accepts a full
//! type to look up extensions: `mime application/json` -> .json

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::LazyLock;

pub struct MimeProvider;

#[derive(Debug, Clone, Copy)]
struct MimeEntry {
    extension: &'static str,
    mime_type: &'static str,
    description: &'static str,
}

const MIMES: &[MimeEntry] = &[
    // Text
    MimeEntry { extension: "txt",   mime_type: "text/plain",                description: "Plain text" },
    MimeEntry { extension: "html",  mime_type: "text/html",                 description: "HTML document" },
    MimeEntry { extension: "htm",   mime_type: "text/html",                 description: "HTML document" },
    MimeEntry { extension: "css",   mime_type: "text/css",                  description: "Cascading Style Sheets" },
    MimeEntry { extension: "csv",   mime_type: "text/csv",                  description: "Comma-separated values" },
    MimeEntry { extension: "md",    mime_type: "text/markdown",             description: "Markdown" },
    MimeEntry { extension: "ics",   mime_type: "text/calendar",             description: "iCalendar" },
    MimeEntry { extension: "vcf",   mime_type: "text/vcard",                description: "vCard contact" },

    // Application data
    MimeEntry { extension: "json",  mime_type: "application/json",          description: "JSON" },
    MimeEntry { extension: "ndjson", mime_type: "application/x-ndjson",     description: "Newline-delimited JSON" },
    MimeEntry { extension: "xml",   mime_type: "application/xml",           description: "XML" },
    MimeEntry { extension: "yaml",  mime_type: "application/yaml",          description: "YAML" },
    MimeEntry { extension: "yml",   mime_type: "application/yaml",          description: "YAML" },
    MimeEntry { extension: "toml",  mime_type: "application/toml",          description: "TOML" },
    MimeEntry { extension: "pdf",   mime_type: "application/pdf",           description: "Portable Document Format" },
    MimeEntry { extension: "rtf",   mime_type: "application/rtf",           description: "Rich Text Format" },
    MimeEntry { extension: "wasm",  mime_type: "application/wasm",          description: "WebAssembly module" },
    MimeEntry { extension: "doc",   mime_type: "application/msword",        description: "Microsoft Word (legacy)" },
    MimeEntry { extension: "docx",  mime_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", description: "Microsoft Word" },
    MimeEntry { extension: "xls",   mime_type: "application/vnd.ms-excel",  description: "Microsoft Excel (legacy)" },
    MimeEntry { extension: "xlsx",  mime_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", description: "Microsoft Excel" },
    MimeEntry { extension: "ppt",   mime_type: "application/vnd.ms-powerpoint", description: "Microsoft PowerPoint (legacy)" },
    MimeEntry { extension: "pptx",  mime_type: "application/vnd.openxmlformats-officedocument.presentationml.presentation", description: "Microsoft PowerPoint" },
    MimeEntry { extension: "odt",   mime_type: "application/vnd.oasis.opendocument.text", description: "OpenDocument Text" },
    MimeEntry { extension: "ods",   mime_type: "application/vnd.oasis.opendocument.spreadsheet", description: "OpenDocument Spreadsheet" },

    // Scripts / source
    MimeEntry { extension: "js",    mime_type: "application/javascript",    description: "JavaScript" },
    MimeEntry { extension: "mjs",   mime_type: "application/javascript",    description: "JavaScript ES module" },
    MimeEntry { extension: "ts",    mime_type: "application/typescript",    description: "TypeScript" },
    MimeEntry { extension: "py",    mime_type: "text/x-python",             description: "Python source" },
    MimeEntry { extension: "rs",    mime_type: "text/rust",                 description: "Rust source" },
    MimeEntry { extension: "go",    mime_type: "text/x-go",                 description: "Go source" },
    MimeEntry { extension: "sh",    mime_type: "application/x-sh",          description: "Shell script" },

    // Images
    MimeEntry { extension: "png",   mime_type: "image/png",                 description: "PNG image" },
    MimeEntry { extension: "jpg",   mime_type: "image/jpeg",                description: "JPEG image" },
    MimeEntry { extension: "jpeg",  mime_type: "image/jpeg",                description: "JPEG image" },
    MimeEntry { extension: "gif",   mime_type: "image/gif",                 description: "GIF image" },
    MimeEntry { extension: "webp",  mime_type: "image/webp",                description: "WebP image" },
    MimeEntry { extension: "avif",  mime_type: "image/avif",                description: "AVIF image" },
    MimeEntry { extension: "svg",   mime_type: "image/svg+xml",             description: "SVG vector image" },
    MimeEntry { extension: "ico",   mime_type: "image/x-icon",              description: "Favicon" },
    MimeEntry { extension: "bmp",   mime_type: "image/bmp",                 description: "Bitmap image" },
    MimeEntry { extension: "tiff",  mime_type: "image/tiff",                description: "TIFF image" },
    MimeEntry { extension: "heic",  mime_type: "image/heic",                description: "HEIC image (Apple)" },

    // Audio / video
    MimeEntry { extension: "mp3",   mime_type: "audio/mpeg",                description: "MP3 audio" },
    MimeEntry { extension: "wav",   mime_type: "audio/wav",                 description: "WAV audio" },
    MimeEntry { extension: "ogg",   mime_type: "audio/ogg",                 description: "Ogg Vorbis audio" },
    MimeEntry { extension: "flac",  mime_type: "audio/flac",                description: "FLAC audio" },
    MimeEntry { extension: "aac",   mime_type: "audio/aac",                 description: "AAC audio" },
    MimeEntry { extension: "m4a",   mime_type: "audio/mp4",                 description: "M4A audio" },
    MimeEntry { extension: "mp4",   mime_type: "video/mp4",                 description: "MP4 video" },
    MimeEntry { extension: "m4v",   mime_type: "video/mp4",                 description: "M4V video" },
    MimeEntry { extension: "mov",   mime_type: "video/quicktime",           description: "QuickTime video" },
    MimeEntry { extension: "webm",  mime_type: "video/webm",                description: "WebM video" },
    MimeEntry { extension: "mkv",   mime_type: "video/x-matroska",          description: "Matroska video" },
    MimeEntry { extension: "avi",   mime_type: "video/x-msvideo",           description: "AVI video" },

    // Archives / binary
    MimeEntry { extension: "zip",   mime_type: "application/zip",           description: "ZIP archive" },
    MimeEntry { extension: "tar",   mime_type: "application/x-tar",         description: "tar archive" },
    MimeEntry { extension: "gz",    mime_type: "application/gzip",          description: "gzip archive" },
    MimeEntry { extension: "bz2",   mime_type: "application/x-bzip2",       description: "bzip2 archive" },
    MimeEntry { extension: "xz",    mime_type: "application/x-xz",          description: "xz archive" },
    MimeEntry { extension: "7z",    mime_type: "application/x-7z-compressed", description: "7-Zip archive" },
    MimeEntry { extension: "rar",   mime_type: "application/vnd.rar",       description: "RAR archive" },
    MimeEntry { extension: "bin",   mime_type: "application/octet-stream",  description: "Arbitrary binary data" },

    // Fonts
    MimeEntry { extension: "ttf",   mime_type: "font/ttf",                  description: "TrueType font" },
    MimeEntry { extension: "otf",   mime_type: "font/otf",                  description: "OpenType font" },
    MimeEntry { extension: "woff",  mime_type: "font/woff",                 description: "Web Open Font Format" },
    MimeEntry { extension: "woff2", mime_type: "font/woff2",                description: "Web Open Font Format 2" },

    // Forms / multipart
    MimeEntry { extension: "form",  mime_type: "application/x-www-form-urlencoded", description: "URL-encoded form data" },
    MimeEntry { extension: "multipart", mime_type: "multipart/form-data",   description: "Multipart form data" },
];

static CACHED_LIST: LazyLock<Vec<Candidate>> =
    LazyLock::new(|| MIMES.iter().map(to_candidate).collect());

#[async_trait]
impl Provider for MimeProvider {
    fn id(&self) -> &str {
        "mime"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = strip_keyword(pattern) else {
            return vec![];
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return CACHED_LIST.clone();
        }

        let needle = rest.trim_start_matches('.').to_lowercase();
        MIMES
            .iter()
            .filter(|m| {
                m.extension == needle
                    || m.mime_type.to_lowercase().contains(&needle)
                    || m.description.to_lowercase().contains(&needle)
            })
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("mime::")
            .ok_or_else(|| anyhow::anyhow!("invalid mime candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    s.strip_prefix("mime ")
        .or_else(|| s.strip_prefix("mimetype "))
        .or_else(|| {
            if s == "mime" || s == "mimetype" {
                Some("")
            } else {
                None
            }
        })
}

fn to_candidate(entry: &MimeEntry) -> Candidate {
    Candidate {
        id: format!("mime::{}", entry.mime_type),
        title: entry.mime_type.into(),
        subtitle: Some(format!(".{} · {}", entry.extension, entry.description)),
        icon: Icon::SfSymbol("doc.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: format!("{} {} {}", entry.extension, entry.mime_type, entry.description),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = MimeProvider;
        assert!(p.query(&Query::new("pdf")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mime")).await;
        assert_eq!(out.len(), MIMES.len());
    }

    #[tokio::test]
    async fn extension_lookup_pdf() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mime pdf")).await;
        assert!(out.iter().any(|c| c.title == "application/pdf"));
    }

    #[tokio::test]
    async fn extension_with_dot() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mime .png")).await;
        assert!(out.iter().any(|c| c.title == "image/png"));
    }

    #[tokio::test]
    async fn type_substring_lookup() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mime image/")).await;
        assert!(out.iter().all(|c| c.title.starts_with("image/")));
        assert!(out.len() >= 5);
    }

    #[tokio::test]
    async fn jpeg_aliases_both_match() {
        let p = MimeProvider;
        let jpg = p.query(&Query::new("mime jpg")).await;
        let jpeg = p.query(&Query::new("mime jpeg")).await;
        assert!(jpg.iter().any(|c| c.title == "image/jpeg"));
        assert!(jpeg.iter().any(|c| c.title == "image/jpeg"));
    }

    #[tokio::test]
    async fn mimetype_alias() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mimetype json")).await;
        assert!(out.iter().any(|c| c.title == "application/json"));
    }

    #[tokio::test]
    async fn case_insensitive() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mime JSON")).await;
        assert!(out.iter().any(|c| c.title == "application/json"));
    }

    #[tokio::test]
    async fn activate_copies_mime_type() {
        let p = MimeProvider;
        let out = p.query(&Query::new("mime pdf")).await;
        let pdf = out.iter().find(|c| c.title == "application/pdf").unwrap();
        let effect = p.activate(&pdf.id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "application/pdf"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = MimeProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn well_known_types_present() {
        let by_ext: std::collections::HashMap<&str, &MimeEntry> =
            MIMES.iter().map(|m| (m.extension, m)).collect();
        assert_eq!(by_ext.get("pdf").unwrap().mime_type, "application/pdf");
        assert_eq!(by_ext.get("json").unwrap().mime_type, "application/json");
        assert_eq!(by_ext.get("png").unwrap().mime_type, "image/png");
        assert_eq!(by_ext.get("svg").unwrap().mime_type, "image/svg+xml");
    }
}
