//! Morse code encoder/decoder. `morse SOS` -> "... --- ..." ;
//! `morsedec ... --- ...` -> "SOS"
//!
//! Letters separated by single space, words by `/` (the most common
//! convention online and the one Devly uses)

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct MorseProvider;

const TABLE: &[(char, &str)] = &[
    ('A', ".-"),
    ('B', "-..."),
    ('C', "-.-."),
    ('D', "-.."),
    ('E', "."),
    ('F', "..-."),
    ('G', "--."),
    ('H', "...."),
    ('I', ".."),
    ('J', ".---"),
    ('K', "-.-"),
    ('L', ".-.."),
    ('M', "--"),
    ('N', "-."),
    ('O', "---"),
    ('P', ".--."),
    ('Q', "--.-"),
    ('R', ".-."),
    ('S', "..."),
    ('T', "-"),
    ('U', "..-"),
    ('V', "...-"),
    ('W', ".--"),
    ('X', "-..-"),
    ('Y', "-.--"),
    ('Z', "--.."),
    ('0', "-----"),
    ('1', ".----"),
    ('2', "..---"),
    ('3', "...--"),
    ('4', "....-"),
    ('5', "....."),
    ('6', "-...."),
    ('7', "--..."),
    ('8', "---.."),
    ('9', "----."),
    ('.', ".-.-.-"),
    (',', "--..--"),
    ('?', "..--.."),
    ('\'', ".----."),
    ('!', "-.-.--"),
    ('/', "-..-."),
    ('(', "-.--."),
    (')', "-.--.-"),
    ('&', ".-..."),
    (':', "---..."),
    (';', "-.-.-."),
    ('=', "-...-"),
    ('+', ".-.-."),
    ('-', "-....-"),
    ('_', "..--.-"),
    ('"', ".-..-."),
    ('@', ".--.-."),
    ('$', "...-..-"),
];

#[async_trait]
impl Provider for MorseProvider {
    fn id(&self) -> &str {
        "morse"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        if let Some(rest) = pattern
            .strip_prefix("morsedec ")
            .or_else(|| pattern.strip_prefix("morsedecode "))
        {
            let rest = rest.trim();
            if rest.is_empty() {
                return vec![];
            }
            let decoded = decode(rest);
            return vec![candidate(&decoded, "morse → text")];
        }
        if let Some(rest) = pattern.strip_prefix("morse ") {
            let rest = rest.trim();
            if rest.is_empty() {
                return vec![];
            }
            let encoded = encode(rest);
            return vec![candidate(&encoded, "text → morse")];
        }
        vec![]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("morse::")
            .ok_or_else(|| anyhow::anyhow!("invalid morse candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

/// Encode plain text as Morse. Each supported character becomes its dits
/// and dahs separated by single space; word boundaries become " / ".
/// Unknown characters become `?` (single dit-dah-dit-dah-dit-dit) - the
/// universal "what?" prosign
pub fn encode(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in text.split([' ', '\t', '\n']) {
        if word.is_empty() {
            continue;
        }
        let mut letters: Vec<&'static str> = Vec::new();
        for ch in word.chars() {
            let upper = ch.to_ascii_uppercase();
            if let Some(&(_, code)) = TABLE.iter().find(|(c, _)| *c == upper) {
                letters.push(code);
            } else {
                letters.push("..--..");
            }
        }
        words.push(letters.join(" "));
    }
    words.join(" / ")
}

/// Decode a Morse string back to text. Letters split on whitespace,
/// words split on `/`. Unknown codes are emitted as `?`
pub fn decode(morse: &str) -> String {
    let mut out = String::new();
    let words = morse.split('/').map(str::trim);
    for (i, word) in words.enumerate() {
        if i > 0 {
            out.push(' ');
        }
        for letter in word.split_whitespace() {
            if let Some(&(c, _)) = TABLE.iter().find(|(_, code)| *code == letter) {
                out.push(c);
            } else if !letter.is_empty() {
                out.push('?');
            }
        }
    }
    out
}

fn candidate(value: &str, label: &str) -> Candidate {
    Candidate {
        id: format!("morse::{value}"),
        title: truncate(value, 200),
        subtitle: Some(label.to_string()),
        icon: Icon::SfSymbol("dot.radiowaves.left.and.right".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
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
        let p = MorseProvider;
        assert!(p.query(&Query::new("SOS")).await.is_empty());
        assert!(p.query(&Query::new("...")).await.is_empty());
    }

    #[tokio::test]
    async fn empty_input_no_match() {
        let p = MorseProvider;
        assert!(p.query(&Query::new("morse ")).await.is_empty());
        assert!(p.query(&Query::new("morsedec ")).await.is_empty());
    }

    #[tokio::test]
    async fn encode_sos() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morse SOS")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "... --- ...");
    }

    #[tokio::test]
    async fn encode_lowercase_treated_as_uppercase() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morse sos")).await;
        assert_eq!(out[0].title, "... --- ...");
    }

    #[tokio::test]
    async fn encode_word_separator() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morse HI BYE")).await;
        assert_eq!(out[0].title, ".... .. / -... -.-- .");
    }

    #[tokio::test]
    async fn decode_sos() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morsedec ... --- ...")).await;
        assert_eq!(out[0].title, "SOS");
    }

    #[tokio::test]
    async fn decode_alias() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morsedecode ... --- ...")).await;
        assert_eq!(out[0].title, "SOS");
    }

    #[tokio::test]
    async fn decode_word_separator() {
        let p = MorseProvider;
        let out = p
            .query(&Query::new("morsedec .... .. / -... -.-- ."))
            .await;
        assert_eq!(out[0].title, "HI BYE");
    }

    #[tokio::test]
    async fn encode_digits() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morse 911")).await;
        assert_eq!(out[0].title, "----. .---- .----");
    }

    #[tokio::test]
    async fn encode_punctuation() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morse Hi!")).await;
        // H I !
        assert_eq!(out[0].title, ".... .. -.-.--");
    }

    #[tokio::test]
    async fn encode_unknown_becomes_question() {
        // Tilde isn't in the table -> should map to "..--.."
        let p = MorseProvider;
        let out = p.query(&Query::new("morse ~")).await;
        assert_eq!(out[0].title, "..--..");
    }

    #[tokio::test]
    async fn decode_unknown_becomes_q() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morsedec ...... ")).await;
        // 6 dots is not a valid letter code -> '?'
        assert_eq!(out[0].title, "?");
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = MorseProvider;
        let out = p.query(&Query::new("morse SOS")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "... --- ..."),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = MorseProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn encode_pure_known_phrases() {
        assert_eq!(encode("HELLO"), ".... . .-.. .-.. ---");
        assert_eq!(encode("PARIS"), ".--. .- .-. .. ...");
        assert_eq!(encode("SOS"), "... --- ...");
    }

    #[test]
    fn encode_handles_extra_whitespace() {
        assert_eq!(encode("  HI   BYE  "), ".... .. / -... -.-- .");
    }

    #[test]
    fn roundtrip_alphabet() {
        let plain: String = ('A'..='Z').collect();
        let coded = encode(&plain);
        let back = decode(&coded);
        assert_eq!(back, plain);
    }

    #[test]
    fn roundtrip_digits() {
        let plain = "0123456789";
        let coded = encode(plain);
        let back = decode(&coded);
        assert_eq!(back, plain);
    }

    #[test]
    fn roundtrip_with_words() {
        let plain = "HELLO WORLD";
        let coded = encode(plain);
        let back = decode(&coded);
        assert_eq!(back, plain);
    }

    #[test]
    fn table_is_unique_in_both_directions() {
        // No duplicate characters
        let chars: std::collections::HashSet<_> = TABLE.iter().map(|(c, _)| *c).collect();
        assert_eq!(chars.len(), TABLE.len());
        // No duplicate codes (otherwise decode is ambiguous)
        let codes: std::collections::HashSet<_> = TABLE.iter().map(|(_, code)| *code).collect();
        assert_eq!(codes.len(), TABLE.len());
    }
}
