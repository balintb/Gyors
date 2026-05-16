//! HTTP status-code reference. `http 404` -> "Not Found - The server can't
//! find requested resource." Bare `http` lists all codes for browsing
//!
//! Numeric input -> exact lookup. Non-numeric -> fuzzy match against the
//! status name (e.g., `http teapot` finds 418)

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::LazyLock;

pub struct HttpStatusProvider;

#[derive(Debug, Clone, Copy)]
struct StatusCode {
    code: u16,
    name: &'static str,
    description: &'static str,
}

const STATUSES: &[StatusCode] = &[
    // 1xx Informational
    StatusCode { code: 100, name: "Continue", description: "Server received request headers; client should send the body." },
    StatusCode { code: 101, name: "Switching Protocols", description: "Server is switching protocols as requested by the client." },
    StatusCode { code: 102, name: "Processing", description: "Server has received the request but is still processing (WebDAV)." },
    StatusCode { code: 103, name: "Early Hints", description: "Used to return preliminary headers before the final response." },

    // 2xx Success
    StatusCode { code: 200, name: "OK", description: "Standard response for a successful HTTP request." },
    StatusCode { code: 201, name: "Created", description: "Request succeeded and a new resource was created." },
    StatusCode { code: 202, name: "Accepted", description: "Request accepted for processing, but not yet completed." },
    StatusCode { code: 203, name: "Non-Authoritative Information", description: "Returned meta-information differs from origin server." },
    StatusCode { code: 204, name: "No Content", description: "Request succeeded; no message body returned." },
    StatusCode { code: 205, name: "Reset Content", description: "Client should reset the document view." },
    StatusCode { code: 206, name: "Partial Content", description: "Server is delivering only part of the resource (Range header)." },
    StatusCode { code: 207, name: "Multi-Status", description: "Body contains multiple statuses for separate operations (WebDAV)." },
    StatusCode { code: 208, name: "Already Reported", description: "Members of a DAV binding already enumerated (WebDAV)." },
    StatusCode { code: 226, name: "IM Used", description: "Server fulfilled the request with instance manipulations applied." },

    // 3xx Redirection
    StatusCode { code: 300, name: "Multiple Choices", description: "Multiple options for the resource; client should choose." },
    StatusCode { code: 301, name: "Moved Permanently", description: "Resource has been permanently moved to a new URL." },
    StatusCode { code: 302, name: "Found", description: "Resource is temporarily at a different URL." },
    StatusCode { code: 303, name: "See Other", description: "Response can be found at another URL using GET." },
    StatusCode { code: 304, name: "Not Modified", description: "Resource hasn't changed since the version specified by If-Modified-Since." },
    StatusCode { code: 305, name: "Use Proxy", description: "Resource must be accessed through a proxy (deprecated)." },
    StatusCode { code: 307, name: "Temporary Redirect", description: "Repeat request to new URL with the same method." },
    StatusCode { code: 308, name: "Permanent Redirect", description: "Permanent redirect; method must not change." },

    // 4xx Client Error
    StatusCode { code: 400, name: "Bad Request", description: "Server can't process the request due to client error (e.g., malformed syntax)." },
    StatusCode { code: 401, name: "Unauthorized", description: "Authentication is required and has failed or not been provided." },
    StatusCode { code: 402, name: "Payment Required", description: "Reserved for future use; rarely used in practice." },
    StatusCode { code: 403, name: "Forbidden", description: "Server understood the request but refuses to authorize it." },
    StatusCode { code: 404, name: "Not Found", description: "Server can't find the requested resource." },
    StatusCode { code: 405, name: "Method Not Allowed", description: "Request method is not supported for the resource." },
    StatusCode { code: 406, name: "Not Acceptable", description: "Resource can't generate content matching the Accept header." },
    StatusCode { code: 407, name: "Proxy Authentication Required", description: "Client must first authenticate with the proxy." },
    StatusCode { code: 408, name: "Request Timeout", description: "Server timed out waiting for the request." },
    StatusCode { code: 409, name: "Conflict", description: "Request conflicts with the current state of the resource." },
    StatusCode { code: 410, name: "Gone", description: "Resource is no longer available and won't return." },
    StatusCode { code: 411, name: "Length Required", description: "Request did not specify the length of its content." },
    StatusCode { code: 412, name: "Precondition Failed", description: "A precondition in the request headers was not met." },
    StatusCode { code: 413, name: "Payload Too Large", description: "Request entity is larger than the server is willing to process." },
    StatusCode { code: 414, name: "URI Too Long", description: "URI provided in the request is too long for the server to process." },
    StatusCode { code: 415, name: "Unsupported Media Type", description: "Server refuses the media format of the request." },
    StatusCode { code: 416, name: "Range Not Satisfiable", description: "Range specified by the Range header can't be fulfilled." },
    StatusCode { code: 417, name: "Expectation Failed", description: "Server can't meet the requirements of the Expect header." },
    StatusCode { code: 418, name: "I'm a teapot", description: "Server refuses to brew coffee with a teapot (RFC 2324, April Fools)." },
    StatusCode { code: 421, name: "Misdirected Request", description: "Request was sent to a server unable to produce a response." },
    StatusCode { code: 422, name: "Unprocessable Entity", description: "Request well-formed but had semantic errors (WebDAV)." },
    StatusCode { code: 423, name: "Locked", description: "Resource that's being accessed is locked (WebDAV)." },
    StatusCode { code: 424, name: "Failed Dependency", description: "Request failed because a previous request failed (WebDAV)." },
    StatusCode { code: 425, name: "Too Early", description: "Server unwilling to risk processing a request that may be replayed." },
    StatusCode { code: 426, name: "Upgrade Required", description: "Client should switch to a different protocol." },
    StatusCode { code: 428, name: "Precondition Required", description: "Origin server requires the request to be conditional." },
    StatusCode { code: 429, name: "Too Many Requests", description: "User has sent too many requests in a given time (rate limiting)." },
    StatusCode { code: 431, name: "Request Header Fields Too Large", description: "Server unwilling to process due to large header fields." },
    StatusCode { code: 451, name: "Unavailable For Legal Reasons", description: "Resource is unavailable for legal reasons (e.g., government censorship)." },

    // 5xx Server Error
    StatusCode { code: 500, name: "Internal Server Error", description: "Generic error message; something has gone wrong on the server." },
    StatusCode { code: 501, name: "Not Implemented", description: "Server doesn't recognize the request method or can't fulfill it." },
    StatusCode { code: 502, name: "Bad Gateway", description: "Server received an invalid response from an upstream server." },
    StatusCode { code: 503, name: "Service Unavailable", description: "Server is temporarily unable to handle the request (overload/maintenance)." },
    StatusCode { code: 504, name: "Gateway Timeout", description: "Server didn't get a response from the upstream server in time." },
    StatusCode { code: 505, name: "HTTP Version Not Supported", description: "Server doesn't support the HTTP protocol version used in the request." },
    StatusCode { code: 506, name: "Variant Also Negotiates", description: "Transparent content negotiation results in a circular reference." },
    StatusCode { code: 507, name: "Insufficient Storage", description: "Server is unable to store the representation needed (WebDAV)." },
    StatusCode { code: 508, name: "Loop Detected", description: "Server detected an infinite loop while processing the request (WebDAV)." },
    StatusCode { code: 510, name: "Not Extended", description: "Further extensions to the request are required for the server to fulfill it." },
    StatusCode { code: 511, name: "Network Authentication Required", description: "Client needs to authenticate to gain network access (e.g., captive portal)." },
];

static CACHED_LIST: LazyLock<Vec<Candidate>> =
    LazyLock::new(|| STATUSES.iter().map(to_candidate).collect());

#[async_trait]
impl Provider for HttpStatusProvider {
    fn id(&self) -> &str {
        "http"
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

        // Numeric -> exact match (must be a complete code)
        if let Ok(code) = rest.parse::<u16>() {
            return STATUSES
                .iter()
                .filter(|s| s.code == code)
                .map(to_candidate)
                .collect();
        }

        // Otherwise fuzzy-substring on the name (case-insensitive)
        let needle = rest.to_lowercase();
        STATUSES
            .iter()
            .filter(|s| s.name.to_lowercase().contains(&needle))
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("http::")
            .ok_or_else(|| anyhow::anyhow!("invalid http candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    s.strip_prefix("http ").or_else(|| {
        if s == "http" {
            Some("")
        } else {
            None
        }
    })
}

fn to_candidate(status: &StatusCode) -> Candidate {
    let value = format!("{} {}", status.code, status.name);
    Candidate {
        id: format!("http::{value}"),
        title: value.clone(),
        subtitle: Some(format!("{} · {}", category(status.code), status.description)),
        icon: Icon::SfSymbol(symbol_for(status.code).into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: format!("{} {} {}", status.code, status.name, status.description),
        bypass_rank: true,
    }
}

fn category(code: u16) -> &'static str {
    match code / 100 {
        1 => "1xx Informational",
        2 => "2xx Success",
        3 => "3xx Redirect",
        4 => "4xx Client Error",
        5 => "5xx Server Error",
        _ => "Unknown",
    }
}

fn symbol_for(code: u16) -> &'static str {
    match code / 100 {
        1 => "info.circle",
        2 => "checkmark.circle.fill",
        3 => "arrow.triangle.turn.up.right.circle.fill",
        4 => "exclamationmark.triangle.fill",
        5 => "xmark.octagon.fill",
        _ => "questionmark.circle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = HttpStatusProvider;
        assert!(p.query(&Query::new("404")).await.is_empty());
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http")).await;
        assert_eq!(out.len(), STATUSES.len());
    }

    #[tokio::test]
    async fn keyword_with_trailing_space_lists_all() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http ")).await;
        assert_eq!(out.len(), STATUSES.len());
    }

    #[tokio::test]
    async fn numeric_lookup_exact() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http 404")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.starts_with("404"));
        assert!(out[0].title.contains("Not Found"));
    }

    #[tokio::test]
    async fn numeric_lookup_unknown() {
        let p = HttpStatusProvider;
        // 999 isn't a registered code
        assert!(p.query(&Query::new("http 999")).await.is_empty());
    }

    #[tokio::test]
    async fn numeric_lookup_does_not_prefix_match() {
        // `http 4` should NOT return all 4xx codes; 4 isn't a code on its own
        let p = HttpStatusProvider;
        assert!(p.query(&Query::new("http 4")).await.is_empty());
    }

    #[tokio::test]
    async fn name_fuzzy_match() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http teapot")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "418 I'm a teapot");
    }

    #[tokio::test]
    async fn name_fuzzy_match_case_insensitive() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http NOT FOUND")).await;
        assert!(out.iter().any(|c| c.title.contains("404")));
    }

    #[tokio::test]
    async fn category_subtitle_includes_class() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http 200")).await;
        assert!(out[0].subtitle.as_deref().unwrap().contains("2xx"));
    }

    #[tokio::test]
    async fn activate_copies_code_and_name() {
        let p = HttpStatusProvider;
        let out = p.query(&Query::new("http 404")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "404 Not Found"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = HttpStatusProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn codes_are_unique() {
        let mut codes: Vec<u16> = STATUSES.iter().map(|s| s.code).collect();
        codes.sort();
        let before = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), before);
    }

    #[test]
    fn category_table() {
        assert_eq!(category(100), "1xx Informational");
        assert_eq!(category(200), "2xx Success");
        assert_eq!(category(301), "3xx Redirect");
        assert_eq!(category(404), "4xx Client Error");
        assert_eq!(category(500), "5xx Server Error");
        assert_eq!(category(999), "Unknown");
    }
}
