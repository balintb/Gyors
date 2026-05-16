//! Color converter. Accepts hex / rgb / hsl / CSS named-color input; shows
//! rows for hex, rgb(), hsl(), and hsv() so any format can be copied

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::LazyLock;

pub struct ColorProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Debug, Clone, Copy)]
struct Hsl {
    h: f32, // 0..360
    s: f32, // 0..1
    l: f32, // 0..1
}

#[derive(Debug, Clone, Copy)]
struct Hsv {
    h: f32, // 0..360
    s: f32, // 0..1
    v: f32, // 0..1
}

#[async_trait]
impl Provider for ColorProvider {
    fn id(&self) -> &str {
        "color"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(rgb) = parse_color(query.pattern()) else {
            return vec![];
        };
        candidates_for(rgb)
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("color::")
            .ok_or_else(|| anyhow::anyhow!("invalid color candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn candidates_for(rgb: Rgb) -> Vec<Candidate> {
    let hsl = rgb_to_hsl(rgb);
    let hsv = rgb_to_hsv(rgb);
    let hex = hex_str(rgb);
    let rgb_s = rgb_str(rgb);
    let hsl_s = hsl_str(hsl);
    let hsv_s = hsv_str(hsv);
    vec![
        make_candidate(&hex, "Hex", &hex),
        make_candidate(&rgb_s, "RGB", &hex),
        make_candidate(&hsl_s, "HSL", &hex),
        make_candidate(&hsv_s, "HSV", &hex),
    ]
}

fn make_candidate(value: &str, label: &str, swatch_hex: &str) -> Candidate {
    Candidate {
        id: format!("color::{value}"),
        title: value.to_string(),
        subtitle: Some(label.to_string()),
        icon: Icon::ColorSwatch(swatch_hex.to_string()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}


fn parse_color(input: &str) -> Option<Rgb> {
    let s = input.trim();
    // Optional `color ` keyword prefix
    let (s, had_color_kw) = match s.strip_prefix("color ") {
        Some(rest) => (rest.trim_start(), true),
        None => (s, false),
    };

    if let Some(rest) = s.strip_prefix('#') {
        return parse_hex(rest);
    }
    if s.starts_with("rgb") {
        return parse_rgb(s);
    }
    if s.starts_with("hsl") {
        return parse_hsl(s);
    }
    // Named CSS colors only fire after the `color` keyword, so plain text
    // like "red" or "white" doesn't accidentally produce a swatch
    if had_color_kw {
        if let Some(rgb) = lookup_named(s) {
            return Some(rgb);
        }
        if looks_like_hex(s) {
            return parse_hex(s);
        }
    }
    None
}

/// CSS named colors (lowercased). Both `aqua` and `cyan` are recognised
/// - the W3C spec lists them as synonymous
static NAMED_COLORS: LazyLock<std::collections::HashMap<&'static str, Rgb>> = LazyLock::new(|| {
    let entries: &[(&str, u8, u8, u8)] = &[
        ("aliceblue", 240, 248, 255),
        ("antiquewhite", 250, 235, 215),
        ("aqua", 0, 255, 255),
        ("aquamarine", 127, 255, 212),
        ("azure", 240, 255, 255),
        ("beige", 245, 245, 220),
        ("bisque", 255, 228, 196),
        ("black", 0, 0, 0),
        ("blanchedalmond", 255, 235, 205),
        ("blue", 0, 0, 255),
        ("blueviolet", 138, 43, 226),
        ("brown", 165, 42, 42),
        ("burlywood", 222, 184, 135),
        ("cadetblue", 95, 158, 160),
        ("chartreuse", 127, 255, 0),
        ("chocolate", 210, 105, 30),
        ("coral", 255, 127, 80),
        ("cornflowerblue", 100, 149, 237),
        ("cornsilk", 255, 248, 220),
        ("crimson", 220, 20, 60),
        ("cyan", 0, 255, 255),
        ("darkblue", 0, 0, 139),
        ("darkcyan", 0, 139, 139),
        ("darkgoldenrod", 184, 134, 11),
        ("darkgray", 169, 169, 169),
        ("darkgrey", 169, 169, 169),
        ("darkgreen", 0, 100, 0),
        ("darkkhaki", 189, 183, 107),
        ("darkmagenta", 139, 0, 139),
        ("darkolivegreen", 85, 107, 47),
        ("darkorange", 255, 140, 0),
        ("darkorchid", 153, 50, 204),
        ("darkred", 139, 0, 0),
        ("darksalmon", 233, 150, 122),
        ("darkseagreen", 143, 188, 143),
        ("darkslateblue", 72, 61, 139),
        ("darkslategray", 47, 79, 79),
        ("darkslategrey", 47, 79, 79),
        ("darkturquoise", 0, 206, 209),
        ("darkviolet", 148, 0, 211),
        ("deeppink", 255, 20, 147),
        ("deepskyblue", 0, 191, 255),
        ("dimgray", 105, 105, 105),
        ("dimgrey", 105, 105, 105),
        ("dodgerblue", 30, 144, 255),
        ("firebrick", 178, 34, 34),
        ("floralwhite", 255, 250, 240),
        ("forestgreen", 34, 139, 34),
        ("fuchsia", 255, 0, 255),
        ("gainsboro", 220, 220, 220),
        ("ghostwhite", 248, 248, 255),
        ("gold", 255, 215, 0),
        ("goldenrod", 218, 165, 32),
        ("gray", 128, 128, 128),
        ("grey", 128, 128, 128),
        ("green", 0, 128, 0),
        ("greenyellow", 173, 255, 47),
        ("honeydew", 240, 255, 240),
        ("hotpink", 255, 105, 180),
        ("indianred", 205, 92, 92),
        ("indigo", 75, 0, 130),
        ("ivory", 255, 255, 240),
        ("khaki", 240, 230, 140),
        ("lavender", 230, 230, 250),
        ("lavenderblush", 255, 240, 245),
        ("lawngreen", 124, 252, 0),
        ("lemonchiffon", 255, 250, 205),
        ("lightblue", 173, 216, 230),
        ("lightcoral", 240, 128, 128),
        ("lightcyan", 224, 255, 255),
        ("lightgoldenrodyellow", 250, 250, 210),
        ("lightgray", 211, 211, 211),
        ("lightgrey", 211, 211, 211),
        ("lightgreen", 144, 238, 144),
        ("lightpink", 255, 182, 193),
        ("lightsalmon", 255, 160, 122),
        ("lightseagreen", 32, 178, 170),
        ("lightskyblue", 135, 206, 250),
        ("lightslategray", 119, 136, 153),
        ("lightslategrey", 119, 136, 153),
        ("lightsteelblue", 176, 196, 222),
        ("lightyellow", 255, 255, 224),
        ("lime", 0, 255, 0),
        ("limegreen", 50, 205, 50),
        ("linen", 250, 240, 230),
        ("magenta", 255, 0, 255),
        ("maroon", 128, 0, 0),
        ("mediumaquamarine", 102, 205, 170),
        ("mediumblue", 0, 0, 205),
        ("mediumorchid", 186, 85, 211),
        ("mediumpurple", 147, 112, 219),
        ("mediumseagreen", 60, 179, 113),
        ("mediumslateblue", 123, 104, 238),
        ("mediumspringgreen", 0, 250, 154),
        ("mediumturquoise", 72, 209, 204),
        ("mediumvioletred", 199, 21, 133),
        ("midnightblue", 25, 25, 112),
        ("mintcream", 245, 255, 250),
        ("mistyrose", 255, 228, 225),
        ("moccasin", 255, 228, 181),
        ("navajowhite", 255, 222, 173),
        ("navy", 0, 0, 128),
        ("oldlace", 253, 245, 230),
        ("olive", 128, 128, 0),
        ("olivedrab", 107, 142, 35),
        ("orange", 255, 165, 0),
        ("orangered", 255, 69, 0),
        ("orchid", 218, 112, 214),
        ("palegoldenrod", 238, 232, 170),
        ("palegreen", 152, 251, 152),
        ("paleturquoise", 175, 238, 238),
        ("palevioletred", 219, 112, 147),
        ("papayawhip", 255, 239, 213),
        ("peachpuff", 255, 218, 185),
        ("peru", 205, 133, 63),
        ("pink", 255, 192, 203),
        ("plum", 221, 160, 221),
        ("powderblue", 176, 224, 230),
        ("purple", 128, 0, 128),
        ("rebeccapurple", 102, 51, 153),
        ("red", 255, 0, 0),
        ("rosybrown", 188, 143, 143),
        ("royalblue", 65, 105, 225),
        ("saddlebrown", 139, 69, 19),
        ("salmon", 250, 128, 114),
        ("sandybrown", 244, 164, 96),
        ("seagreen", 46, 139, 87),
        ("seashell", 255, 245, 238),
        ("sienna", 160, 82, 45),
        ("silver", 192, 192, 192),
        ("skyblue", 135, 206, 235),
        ("slateblue", 106, 90, 205),
        ("slategray", 112, 128, 144),
        ("slategrey", 112, 128, 144),
        ("snow", 255, 250, 250),
        ("springgreen", 0, 255, 127),
        ("steelblue", 70, 130, 180),
        ("tan", 210, 180, 140),
        ("teal", 0, 128, 128),
        ("thistle", 216, 191, 216),
        ("tomato", 255, 99, 71),
        ("turquoise", 64, 224, 208),
        ("violet", 238, 130, 238),
        ("wheat", 245, 222, 179),
        ("white", 255, 255, 255),
        ("whitesmoke", 245, 245, 245),
        ("yellow", 255, 255, 0),
        ("yellowgreen", 154, 205, 50),
    ];
    entries
        .iter()
        .map(|(name, r, g, b)| (*name, Rgb { r: *r, g: *g, b: *b }))
        .collect()
});

fn lookup_named(s: &str) -> Option<Rgb> {
    if s.is_empty() {
        return None;
    }
    NAMED_COLORS.get(s.to_lowercase().as_str()).copied()
}

fn looks_like_hex(s: &str) -> bool {
    (s.len() == 3 || s.len() == 6) && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.to_lowercase();
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let full = match s.len() {
        3 => s.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => s,
        _ => return None,
    };
    let r = u8::from_str_radix(&full[0..2], 16).ok()?;
    let g = u8::from_str_radix(&full[2..4], 16).ok()?;
    let b = u8::from_str_radix(&full[4..6], 16).ok()?;
    Some(Rgb { r, g, b })
}

fn parse_rgb(s: &str) -> Option<Rgb> {
    let rest = s.strip_prefix("rgb")?.trim_start();
    let rest = rest.strip_prefix('(').unwrap_or(rest);
    let rest = rest.strip_suffix(')').unwrap_or(rest);
    let parts: Vec<&str> = rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() != 3 {
        return None;
    }
    let r: u8 = parts[0].parse().ok()?;
    let g: u8 = parts[1].parse().ok()?;
    let b: u8 = parts[2].parse().ok()?;
    Some(Rgb { r, g, b })
}

fn parse_hsl(s: &str) -> Option<Rgb> {
    let rest = s.strip_prefix("hsl")?.trim_start();
    let rest = rest.strip_prefix('(').unwrap_or(rest);
    let rest = rest.strip_suffix(')').unwrap_or(rest);
    let parts: Vec<&str> = rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() != 3 {
        return None;
    }
    let h: f32 = parts[0].trim_end_matches('°').parse().ok()?;
    let s_val: f32 = parts[1].trim_end_matches('%').parse().ok()?;
    let l_val: f32 = parts[2].trim_end_matches('%').parse().ok()?;
    // Accept either 0..1 or 0..100 for s/l
    let s_norm = if s_val > 1.0 { s_val / 100.0 } else { s_val };
    let l_norm = if l_val > 1.0 { l_val / 100.0 } else { l_val };
    if !(0.0..=1.0).contains(&s_norm) || !(0.0..=1.0).contains(&l_norm) {
        return None;
    }
    Some(hsl_to_rgb(Hsl { h, s: s_norm, l: l_norm }))
}


fn hex_str(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)
}

fn rgb_str(rgb: Rgb) -> String {
    format!("rgb({}, {}, {})", rgb.r, rgb.g, rgb.b)
}

fn hsl_str(hsl: Hsl) -> String {
    format!(
        "hsl({:.0}, {:.0}%, {:.0}%)",
        hsl.h,
        hsl.s * 100.0,
        hsl.l * 100.0
    )
}

fn hsv_str(hsv: Hsv) -> String {
    format!(
        "hsv({:.0}, {:.0}%, {:.0}%)",
        hsv.h,
        hsv.s * 100.0,
        hsv.v * 100.0
    )
}


fn rgb_to_hsl(rgb: Rgb) -> Hsl {
    let r = rgb.r as f32 / 255.0;
    let g = rgb.g as f32 / 255.0;
    let b = rgb.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return Hsl { h: 0.0, s: 0.0, l };
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    Hsl { h: h * 360.0, s, l }
}

fn rgb_to_hsv(rgb: Rgb) -> Hsv {
    let r = rgb.r as f32 / 255.0;
    let g = rgb.g as f32 / 255.0;
    let b = rgb.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    let d = max - min;
    if max < 1e-6 {
        return Hsv { h: 0.0, s: 0.0, v: 0.0 };
    }
    let s = d / max;
    if d < 1e-6 {
        return Hsv { h: 0.0, s, v };
    }
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    Hsv { h: h * 360.0, s, v }
}

fn hsl_to_rgb(hsl: Hsl) -> Rgb {
    let h = (hsl.h % 360.0 + 360.0) % 360.0 / 360.0;
    let s = hsl.s.clamp(0.0, 1.0);
    let l = hsl.l.clamp(0.0, 1.0);
    if s < 1e-6 {
        let v = (l * 255.0).round() as u8;
        return Rgb { r: v, g: v, b: v };
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    Rgb {
        r: (r * 255.0).round().clamp(0.0, 255.0) as u8,
        g: (g * 255.0).round().clamp(0.0, 255.0) as u8,
        b: (b * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

fn hue_to_rgb(p: f32, q: f32, t: f32) -> f32 {
    let mut t = t;
    if t < 0.0 { t += 1.0; }
    if t > 1.0 { t -= 1.0; }
    if t < 1.0 / 6.0 { return p + (q - p) * 6.0 * t; }
    if t < 1.0 / 2.0 { return q; }
    if t < 2.0 / 3.0 { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn non_color_input_no_results() {
        let p = ColorProvider;
        assert!(p.query(&Query::new("hello world")).await.is_empty());
        assert!(p.query(&Query::new("abc")).await.is_empty());
    }

    #[tokio::test]
    async fn hex_with_hash_triggers() {
        let p = ColorProvider;
        let out = p.query(&Query::new("#ff5733")).await;
        assert_eq!(out.len(), 4);
        assert!(out.iter().any(|c| c.title == "#ff5733"));
        assert!(out.iter().any(|c| c.title == "rgb(255, 87, 51)"));
    }

    #[tokio::test]
    async fn short_hex_expands() {
        let p = ColorProvider;
        let out = p.query(&Query::new("#abc")).await;
        assert!(out.iter().any(|c| c.title == "#aabbcc"));
    }

    #[tokio::test]
    async fn rgb_paren_syntax() {
        let p = ColorProvider;
        let out = p.query(&Query::new("rgb(255, 87, 51)")).await;
        assert!(out.iter().any(|c| c.title == "#ff5733"));
    }

    #[tokio::test]
    async fn rgb_space_syntax() {
        let p = ColorProvider;
        let out = p.query(&Query::new("rgb 255 87 51")).await;
        assert!(out.iter().any(|c| c.title == "#ff5733"));
    }

    #[tokio::test]
    async fn hsl_input() {
        let p = ColorProvider;
        let out = p.query(&Query::new("hsl(11, 100%, 60%)")).await;
        // Should be approximately #ff5733 (11 hue, full saturation, 60% lightness)
        let hex_row = out.iter().find(|c| c.title.starts_with('#')).unwrap();
        assert!(hex_row.title.starts_with("#ff"), "got {}", hex_row.title);
    }

    #[tokio::test]
    async fn color_keyword_with_bare_hex() {
        let p = ColorProvider;
        let out = p.query(&Query::new("color ff5733")).await;
        assert!(out.iter().any(|c| c.title == "#ff5733"));
    }

    #[tokio::test]
    async fn activate_yields_copy_of_value() {
        let p = ColorProvider;
        let out = p.query(&Query::new("#ff5733")).await;
        let hex = out.iter().find(|c| c.title == "#ff5733").unwrap();
        let effect = p.activate(&hex.id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "#ff5733"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = ColorProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn parse_hex_valid() {
        assert_eq!(parse_hex("ff5733"), Some(Rgb { r: 255, g: 87, b: 51 }));
        assert_eq!(parse_hex("abc"), Some(Rgb { r: 170, g: 187, b: 204 }));
        assert_eq!(parse_hex("FFFFFF"), Some(Rgb { r: 255, g: 255, b: 255 }));
        assert_eq!(parse_hex("000000"), Some(Rgb { r: 0, g: 0, b: 0 }));
    }

    #[test]
    fn parse_hex_invalid() {
        assert!(parse_hex("").is_none());
        assert!(parse_hex("xyz").is_none());
        assert!(parse_hex("abcd").is_none()); // wrong length
        assert!(parse_hex("gghhii").is_none()); // not hex
    }

    #[test]
    fn hsl_roundtrip() {
        let rgb = Rgb { r: 255, g: 87, b: 51 };
        let hsl = rgb_to_hsl(rgb);
        let back = hsl_to_rgb(hsl);
        // Allow +/-2 per channel due to floating point rounding
        assert!((back.r as i16 - rgb.r as i16).abs() <= 2);
        assert!((back.g as i16 - rgb.g as i16).abs() <= 2);
        assert!((back.b as i16 - rgb.b as i16).abs() <= 2);
    }

    #[test]
    fn hsv_for_pure_red() {
        let hsv = rgb_to_hsv(Rgb { r: 255, g: 0, b: 0 });
        assert!((hsv.h - 0.0).abs() < 1e-3);
        assert!((hsv.s - 1.0).abs() < 1e-3);
        assert!((hsv.v - 1.0).abs() < 1e-3);
    }

    #[test]
    fn hsl_for_white_and_black() {
        let white = rgb_to_hsl(Rgb { r: 255, g: 255, b: 255 });
        assert!((white.l - 1.0).abs() < 1e-3);
        let black = rgb_to_hsl(Rgb { r: 0, g: 0, b: 0 });
        assert!((black.l - 0.0).abs() < 1e-3);
    }


    #[tokio::test]
    async fn named_color_red_with_keyword() {
        let p = ColorProvider;
        let out = p.query(&Query::new("color red")).await;
        assert!(out.iter().any(|c| c.title == "#ff0000"));
        assert!(out.iter().any(|c| c.title == "rgb(255, 0, 0)"));
    }

    #[tokio::test]
    async fn named_color_case_insensitive() {
        let p = ColorProvider;
        let out = p.query(&Query::new("color REBECCAPURPLE")).await;
        assert!(out.iter().any(|c| c.title == "#663399"));
    }

    #[tokio::test]
    async fn named_color_grey_alias() {
        let p = ColorProvider;
        let g1 = p.query(&Query::new("color gray")).await;
        let g2 = p.query(&Query::new("color grey")).await;
        let h1 = g1.iter().find(|c| c.title.starts_with('#')).unwrap();
        let h2 = g2.iter().find(|c| c.title.starts_with('#')).unwrap();
        assert_eq!(h1.title, h2.title);
    }

    #[tokio::test]
    async fn unknown_named_color_no_results() {
        let p = ColorProvider;
        assert!(p.query(&Query::new("color notarealcolor")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_named_color_without_keyword_no_results() {
        // Without the `color ` prefix, "red" must not produce a swatch -
        // it would steal queries that mean something else
        let p = ColorProvider;
        assert!(p.query(&Query::new("red")).await.is_empty());
    }

    #[test]
    fn lookup_named_table() {
        assert_eq!(
            lookup_named("red"),
            Some(Rgb { r: 255, g: 0, b: 0 })
        );
        assert_eq!(
            lookup_named("WHITE"),
            Some(Rgb { r: 255, g: 255, b: 255 })
        );
        assert_eq!(
            lookup_named("rebeccapurple"),
            Some(Rgb { r: 102, g: 51, b: 153 })
        );
        assert_eq!(lookup_named(""), None);
        assert_eq!(lookup_named("zzz"), None);
    }
}
