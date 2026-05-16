//! TCP/UDP port reference. `port 22` -> SSH; `port ssh` -> 22.
//! Bare `port` lists the well-known ports for browsing

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::LazyLock;

pub struct PortProvider;

#[derive(Debug, Clone, Copy)]
struct PortEntry {
    port: u16,
    name: &'static str,
    description: &'static str,
}

const PORTS: &[PortEntry] = &[
    PortEntry { port: 20,    name: "FTP-data",        description: "File Transfer Protocol - data channel" },
    PortEntry { port: 21,    name: "FTP",             description: "File Transfer Protocol - control channel" },
    PortEntry { port: 22,    name: "SSH",             description: "Secure Shell · scp · sftp · rsync over SSH" },
    PortEntry { port: 23,    name: "Telnet",          description: "Telnet - unencrypted remote login (legacy)" },
    PortEntry { port: 25,    name: "SMTP",            description: "Simple Mail Transfer Protocol - server-to-server mail" },
    PortEntry { port: 53,    name: "DNS",             description: "Domain Name System - name resolution" },
    PortEntry { port: 67,    name: "DHCP",            description: "DHCP - server (BOOTP)" },
    PortEntry { port: 68,    name: "DHCP",            description: "DHCP - client" },
    PortEntry { port: 69,    name: "TFTP",            description: "Trivial File Transfer Protocol" },
    PortEntry { port: 80,    name: "HTTP",            description: "Hypertext Transfer Protocol - plain web" },
    PortEntry { port: 110,   name: "POP3",            description: "Post Office Protocol v3 - receive mail" },
    PortEntry { port: 119,   name: "NNTP",            description: "Network News Transfer Protocol - Usenet" },
    PortEntry { port: 123,   name: "NTP",             description: "Network Time Protocol - clock synchronization" },
    PortEntry { port: 143,   name: "IMAP",            description: "Internet Message Access Protocol - receive mail" },
    PortEntry { port: 161,   name: "SNMP",            description: "Simple Network Management Protocol" },
    PortEntry { port: 162,   name: "SNMP-trap",       description: "SNMP trap - async device alerts" },
    PortEntry { port: 194,   name: "IRC",             description: "Internet Relay Chat" },
    PortEntry { port: 220,   name: "IMAPv3",          description: "IMAP version 3 (rare)" },
    PortEntry { port: 389,   name: "LDAP",            description: "Lightweight Directory Access Protocol" },
    PortEntry { port: 443,   name: "HTTPS",           description: "HTTP over TLS - secure web" },
    PortEntry { port: 445,   name: "SMB",             description: "Server Message Block - Windows file/printer sharing" },
    PortEntry { port: 465,   name: "SMTPS",           description: "SMTP over TLS - implicit TLS submission" },
    PortEntry { port: 514,   name: "Syslog",          description: "Syslog - system log shipping (UDP)" },
    PortEntry { port: 515,   name: "LPD",             description: "Line Printer Daemon" },
    PortEntry { port: 587,   name: "SMTP-submission", description: "SMTP submission - client-to-server with STARTTLS" },
    PortEntry { port: 631,   name: "IPP",             description: "Internet Printing Protocol - CUPS" },
    PortEntry { port: 636,   name: "LDAPS",           description: "LDAP over TLS" },
    PortEntry { port: 873,   name: "rsync",           description: "rsync - daemon mode" },
    PortEntry { port: 989,   name: "FTPS-data",       description: "FTP over TLS - data channel" },
    PortEntry { port: 990,   name: "FTPS",            description: "FTP over TLS - control channel" },
    PortEntry { port: 993,   name: "IMAPS",           description: "IMAP over TLS" },
    PortEntry { port: 995,   name: "POP3S",           description: "POP3 over TLS" },
    PortEntry { port: 1080,  name: "SOCKS",           description: "SOCKS proxy" },
    PortEntry { port: 1194,  name: "OpenVPN",         description: "OpenVPN default" },
    PortEntry { port: 1433,  name: "MSSQL",           description: "Microsoft SQL Server" },
    PortEntry { port: 1521,  name: "Oracle",          description: "Oracle Database default listener" },
    PortEntry { port: 1701,  name: "L2TP",            description: "Layer 2 Tunneling Protocol - VPN" },
    PortEntry { port: 1723,  name: "PPTP",            description: "Point-to-Point Tunneling Protocol - VPN (legacy)" },
    PortEntry { port: 1883,  name: "MQTT",            description: "MQTT - IoT pub/sub messaging" },
    PortEntry { port: 2049,  name: "NFS",             description: "Network File System" },
    PortEntry { port: 2375,  name: "Docker",          description: "Docker daemon - unencrypted (do NOT expose)" },
    PortEntry { port: 2376,  name: "Docker-TLS",      description: "Docker daemon over TLS" },
    PortEntry { port: 2379,  name: "etcd",            description: "etcd client API" },
    PortEntry { port: 2380,  name: "etcd-peer",       description: "etcd peer-to-peer" },
    PortEntry { port: 3000,  name: "dev",             description: "Common dev server (Node, Rails, Grafana)" },
    PortEntry { port: 3001,  name: "dev",             description: "Common dev server alternate" },
    PortEntry { port: 3306,  name: "MySQL",           description: "MySQL / MariaDB" },
    PortEntry { port: 3389,  name: "RDP",             description: "Remote Desktop Protocol" },
    PortEntry { port: 4000,  name: "dev",             description: "Common dev server (Phoenix, Jekyll)" },
    PortEntry { port: 4040,  name: "Spark-UI",        description: "Apache Spark application UI" },
    PortEntry { port: 4200,  name: "Angular",         description: "Angular CLI dev server" },
    PortEntry { port: 4444,  name: "Selenium",        description: "Selenium WebDriver hub" },
    PortEntry { port: 5000,  name: "dev",             description: "Common dev server (Flask, UPnP)" },
    PortEntry { port: 5060,  name: "SIP",             description: "Session Initiation Protocol - VoIP" },
    PortEntry { port: 5061,  name: "SIPS",            description: "SIP over TLS" },
    PortEntry { port: 5173,  name: "Vite",            description: "Vite dev server default" },
    PortEntry { port: 5222,  name: "XMPP",            description: "XMPP client-to-server" },
    PortEntry { port: 5353,  name: "mDNS",            description: "Multicast DNS - Bonjour / zeroconf" },
    PortEntry { port: 5432,  name: "PostgreSQL",      description: "PostgreSQL database" },
    PortEntry { port: 5601,  name: "Kibana",          description: "Kibana web UI" },
    PortEntry { port: 5672,  name: "AMQP",            description: "AMQP - RabbitMQ" },
    PortEntry { port: 5900,  name: "VNC",             description: "VNC remote desktop" },
    PortEntry { port: 5984,  name: "CouchDB",         description: "Apache CouchDB" },
    PortEntry { port: 6379,  name: "Redis",           description: "Redis in-memory data store" },
    PortEntry { port: 6443,  name: "k8s-API",         description: "Kubernetes API server" },
    PortEntry { port: 6667,  name: "IRC",             description: "Internet Relay Chat - common alt" },
    PortEntry { port: 8000,  name: "dev",             description: "Common dev server (Django, http.server)" },
    PortEntry { port: 8080,  name: "HTTP-alt",        description: "Common HTTP alternate / Tomcat default" },
    PortEntry { port: 8086,  name: "InfluxDB",        description: "InfluxDB HTTP API" },
    PortEntry { port: 8443,  name: "HTTPS-alt",       description: "Common HTTPS alternate" },
    PortEntry { port: 8500,  name: "Consul",          description: "HashiCorp Consul HTTP API" },
    PortEntry { port: 8888,  name: "dev",             description: "Common dev server / Jupyter" },
    PortEntry { port: 9000,  name: "dev",             description: "Common dev server (SonarQube, php-fpm)" },
    PortEntry { port: 9090,  name: "Prometheus",      description: "Prometheus metrics" },
    PortEntry { port: 9092,  name: "Kafka",           description: "Apache Kafka" },
    PortEntry { port: 9200,  name: "Elasticsearch",   description: "Elasticsearch HTTP API" },
    PortEntry { port: 9300,  name: "Elasticsearch",   description: "Elasticsearch transport" },
    PortEntry { port: 11211, name: "memcached",       description: "memcached" },
    PortEntry { port: 15672, name: "RabbitMQ",        description: "RabbitMQ management UI" },
    PortEntry { port: 25565, name: "Minecraft",       description: "Minecraft Java Edition server" },
    PortEntry { port: 27017, name: "MongoDB",         description: "MongoDB database" },
    PortEntry { port: 27018, name: "MongoDB-shard",   description: "MongoDB shard server" },
    PortEntry { port: 27019, name: "MongoDB-config",  description: "MongoDB config server" },
];

static CACHED_LIST: LazyLock<Vec<Candidate>> =
    LazyLock::new(|| PORTS.iter().map(to_candidate).collect());

#[async_trait]
impl Provider for PortProvider {
    fn id(&self) -> &str {
        "port"
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

        // Numeric -> exact port number
        if let Ok(port) = rest.parse::<u16>() {
            return PORTS
                .iter()
                .filter(|p| p.port == port)
                .map(to_candidate)
                .collect();
        }

        // Otherwise fuzzy on name/description (case-insensitive substring)
        let needle = rest.to_lowercase();
        PORTS
            .iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.description.to_lowercase().contains(&needle)
            })
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("port::")
            .ok_or_else(|| anyhow::anyhow!("invalid port candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    s.strip_prefix("port ").or_else(|| {
        if s == "port" {
            Some("")
        } else {
            None
        }
    })
}

fn to_candidate(entry: &PortEntry) -> Candidate {
    Candidate {
        id: format!("port::{}", entry.port),
        title: format!("{} - {}", entry.port, entry.name),
        subtitle: Some(entry.description.into()),
        icon: Icon::SfSymbol("network".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: format!("{} {} {}", entry.port, entry.name, entry.description),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = PortProvider;
        assert!(p.query(&Query::new("22")).await.is_empty());
        assert!(p.query(&Query::new("ssh")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all() {
        let p = PortProvider;
        let out = p.query(&Query::new("port")).await;
        assert_eq!(out.len(), PORTS.len());
    }

    #[tokio::test]
    async fn numeric_lookup_finds_ssh() {
        let p = PortProvider;
        let out = p.query(&Query::new("port 22")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("SSH"));
    }

    #[tokio::test]
    async fn numeric_lookup_unknown_port() {
        let p = PortProvider;
        assert!(p.query(&Query::new("port 12345")).await.is_empty());
    }

    #[tokio::test]
    async fn name_lookup_finds_port_number() {
        let p = PortProvider;
        let out = p.query(&Query::new("port ssh")).await;
        assert!(out.iter().any(|c| c.title.starts_with("22 ")));
    }

    #[tokio::test]
    async fn name_lookup_case_insensitive() {
        let p = PortProvider;
        let out = p.query(&Query::new("port HTTPS")).await;
        assert!(out.iter().any(|c| c.title.starts_with("443 ")));
    }

    #[tokio::test]
    async fn description_substring_finds_match() {
        let p = PortProvider;
        let out = p.query(&Query::new("port redis")).await;
        assert!(out.iter().any(|c| c.title.starts_with("6379 ")));
    }

    #[tokio::test]
    async fn activate_copies_port_number() {
        let p = PortProvider;
        let out = p.query(&Query::new("port 22")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "22"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = PortProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn ports_have_no_obvious_typos_in_well_known_set() {
        // Sanity: a handful of canonical ports must be present and correct
        let by_port: std::collections::HashMap<u16, &PortEntry> =
            PORTS.iter().map(|p| (p.port, p)).collect();
        assert_eq!(by_port.get(&22).unwrap().name, "SSH");
        assert_eq!(by_port.get(&80).unwrap().name, "HTTP");
        assert_eq!(by_port.get(&443).unwrap().name, "HTTPS");
        assert_eq!(by_port.get(&5432).unwrap().name, "PostgreSQL");
        assert_eq!(by_port.get(&6379).unwrap().name, "Redis");
        assert_eq!(by_port.get(&27017).unwrap().name, "MongoDB");
    }
}
