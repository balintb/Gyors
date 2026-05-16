//! DNS record-type reference. `dns A` -> "Address record (IPv4)";
//! bare `dns` lists all common record types

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::LazyLock;

pub struct DnsProvider;

#[derive(Debug, Clone, Copy)]
struct DnsRecord {
    name: &'static str,
    title: &'static str,
    description: &'static str,
}

const RECORDS: &[DnsRecord] = &[
    DnsRecord { name: "A",      title: "Address record",          description: "Maps a hostname to an IPv4 address." },
    DnsRecord { name: "AAAA",   title: "IPv6 address record",     description: "Maps a hostname to an IPv6 address." },
    DnsRecord { name: "CNAME",  title: "Canonical name",          description: "Aliases one hostname to another (the canonical name)." },
    DnsRecord { name: "MX",     title: "Mail exchange",           description: "Routes email for a domain to one or more mail servers, with priority." },
    DnsRecord { name: "TXT",    title: "Text record",             description: "Free-form text. Used for SPF, DKIM, DMARC, domain verification, etc." },
    DnsRecord { name: "NS",     title: "Name server",             description: "Delegates a DNS zone to a set of authoritative name servers." },
    DnsRecord { name: "SOA",    title: "Start of authority",      description: "Authoritative info about a zone: primary NS, contact, refresh/expiry/TTL." },
    DnsRecord { name: "PTR",    title: "Pointer (reverse DNS)",   description: "Maps an IP address back to a hostname (used in in-addr.arpa)." },
    DnsRecord { name: "SRV",    title: "Service locator",         description: "Specifies host + port for a named service (e.g., _sip._tcp)." },
    DnsRecord { name: "CAA",    title: "Certification Authority Authorization", description: "Lists CAs allowed to issue certificates for the domain." },
    DnsRecord { name: "DNSKEY", title: "DNS public key (DNSSEC)", description: "Public key used by resolvers to verify DNSSEC signatures." },
    DnsRecord { name: "DS",     title: "Delegation signer (DNSSEC)", description: "Hash of a child-zone DNSKEY, published in the parent zone." },
    DnsRecord { name: "RRSIG",  title: "Resource record signature", description: "DNSSEC signature over an RRset, generated using a DNSKEY private key." },
    DnsRecord { name: "NSEC",   title: "Next secure record",      description: "Proves nonexistence of a name in a zone (DNSSEC)." },
    DnsRecord { name: "NSEC3",  title: "Hashed next secure record", description: "Like NSEC, but uses hashed names to prevent zone enumeration." },
    DnsRecord { name: "TLSA",   title: "TLS authentication via DNS (DANE)", description: "Associates a TLS server certificate with the domain (DANE)." },
    DnsRecord { name: "SVCB",   title: "Service binding",         description: "Generic service-binding record (parameters for a service endpoint)." },
    DnsRecord { name: "HTTPS",  title: "HTTPS service binding",   description: "Service-binding record for HTTPS endpoints (ALPN, ECH, IP hints)." },
    DnsRecord { name: "SPF",    title: "Sender Policy Framework", description: "Authorized mail servers for a domain (modern usage is via TXT)." },
    DnsRecord { name: "HINFO",  title: "Host information",        description: "CPU and OS info for a host (rarely used in practice)." },
    DnsRecord { name: "LOC",    title: "Location",                description: "Geographic location (latitude / longitude / altitude)." },
    DnsRecord { name: "NAPTR",  title: "Naming Authority Pointer", description: "URI/regex rewriting rules used in ENUM and SIP discovery." },
    DnsRecord { name: "AXFR",   title: "Full zone transfer",      description: "Pseudo-record requesting a complete zone transfer from primary." },
    DnsRecord { name: "IXFR",   title: "Incremental zone transfer", description: "Pseudo-record requesting only changes since a serial number." },
    DnsRecord { name: "ANY",    title: "Any record",              description: "Pseudo-query for all record types (rate-limited or refused by many resolvers)." },
];

static CACHED_LIST: LazyLock<Vec<Candidate>> =
    LazyLock::new(|| RECORDS.iter().map(to_candidate).collect());

#[async_trait]
impl Provider for DnsProvider {
    fn id(&self) -> &str {
        "dns"
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

        let needle = rest.to_lowercase();
        // Exact name match first
        let exact: Vec<Candidate> = RECORDS
            .iter()
            .filter(|r| r.name.to_lowercase() == needle)
            .map(to_candidate)
            .collect();
        if !exact.is_empty() {
            return exact;
        }

        // Otherwise prefix on name, then substring across name/title/desc
        let prefix: Vec<Candidate> = RECORDS
            .iter()
            .filter(|r| r.name.to_lowercase().starts_with(&needle))
            .map(to_candidate)
            .collect();
        if !prefix.is_empty() {
            return prefix;
        }

        RECORDS
            .iter()
            .filter(|r| {
                r.name.to_lowercase().contains(&needle)
                    || r.title.to_lowercase().contains(&needle)
                    || r.description.to_lowercase().contains(&needle)
            })
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("dns::")
            .ok_or_else(|| anyhow::anyhow!("invalid dns candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    s.strip_prefix("dns ").or_else(|| {
        if s == "dns" {
            Some("")
        } else {
            None
        }
    })
}

fn to_candidate(record: &DnsRecord) -> Candidate {
    Candidate {
        id: format!("dns::{}", record.name),
        title: format!("{} - {}", record.name, record.title),
        subtitle: Some(record.description.into()),
        icon: Icon::SfSymbol("network".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: format!("{} {} {}", record.name, record.title, record.description),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = DnsProvider;
        assert!(p.query(&Query::new("A")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all() {
        let p = DnsProvider;
        let out = p.query(&Query::new("dns")).await;
        assert_eq!(out.len(), RECORDS.len());
    }

    #[tokio::test]
    async fn exact_name_match_is_unique() {
        // `dns A` should match ONLY the A record, not also AAAA / ANY
        let p = DnsProvider;
        let out = p.query(&Query::new("dns A")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.starts_with("A "));
    }

    #[tokio::test]
    async fn case_insensitive_exact() {
        let p = DnsProvider;
        let out = p.query(&Query::new("dns mx")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.starts_with("MX "));
    }

    #[tokio::test]
    async fn prefix_fallback_when_no_exact() {
        // `dns AA` - no exact, but AAAA prefix-matches
        let p = DnsProvider;
        let out = p.query(&Query::new("dns AA")).await;
        assert!(out.iter().any(|c| c.title.starts_with("AAAA ")));
    }

    #[tokio::test]
    async fn description_substring_fallback() {
        let p = DnsProvider;
        let out = p.query(&Query::new("dns ipv6")).await;
        assert!(out.iter().any(|c| c.title.starts_with("AAAA ")));
    }

    #[tokio::test]
    async fn dnssec_keyword_in_descriptions() {
        let p = DnsProvider;
        let out = p.query(&Query::new("dns dnssec")).await;
        assert!(out.iter().any(|c| c.title.starts_with("DNSKEY ")));
        assert!(out.iter().any(|c| c.title.starts_with("RRSIG ")));
    }

    #[tokio::test]
    async fn activate_copies_record_name() {
        let p = DnsProvider;
        let out = p.query(&Query::new("dns A")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "A"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = DnsProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn record_names_are_unique() {
        let mut names: Vec<&str> = RECORDS.iter().map(|r| r.name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
