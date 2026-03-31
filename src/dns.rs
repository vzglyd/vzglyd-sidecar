use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::Error;
use crate::http::{https_get_with_candidates, successful_body};

const DOH_HOST: &str = "dns.google";
const DNS_CACHE_TTL_FLOOR_SECS: u64 = 30;
const DNS_CACHE_TTL_CAP_SECS: u64 = 300;
const DNS_STATUS_NOERROR: u32 = 0;
const DNS_RECORD_A: u32 = 1;
const DNS_RECORD_CNAME: u32 = 5;
const DNS_MAX_CNAME_DEPTH: usize = 4;
const DOH_BOOTSTRAP_IPS: [Ipv4Addr; 2] = [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(8, 8, 4, 4)];

#[derive(Clone)]
struct CachedResolution {
    addrs: Vec<Ipv4Addr>,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct DnsJsonResponse {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answers: Vec<DnsAnswer>,
}

#[derive(Deserialize)]
struct DnsAnswer {
    #[serde(rename = "type")]
    record_type: u32,
    #[serde(rename = "TTL")]
    ttl: Option<u32>,
    data: String,
}

pub(crate) struct DnsResolver {
    cache: HashMap<String, CachedResolution>,
}

impl DnsResolver {
    pub(crate) fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub(crate) fn resolve(&mut self, host: &str) -> Result<Vec<Ipv4Addr>, Error> {
        let now = Instant::now();
        if let Some(entry) = self.cache.get(host) {
            if entry.expires_at > now {
                return Ok(entry.addrs.clone());
            }
        }
        self.cache.remove(host);

        let (addrs, ttl) = self.resolve_host_v4(host, 0)?;
        let expires_at = Instant::now().checked_add(ttl).unwrap_or_else(Instant::now);
        self.cache.insert(
            host.to_string(),
            CachedResolution {
                addrs: addrs.clone(),
                expires_at,
            },
        );
        Ok(addrs)
    }

    fn resolve_host_v4(
        &self,
        host: &str,
        depth: usize,
    ) -> Result<(Vec<Ipv4Addr>, Duration), Error> {
        if depth >= DNS_MAX_CNAME_DEPTH {
            return Err(Error::Dns(format!(
                "DNS lookup for '{host}' exceeded CNAME depth limit"
            )));
        }

        let path = format!("/resolve?name={host}&type=A");
        let headers = vec![("Accept".to_string(), "application/json".to_string())];
        let body = successful_body(https_get_with_candidates(
            DOH_HOST,
            &path,
            &headers,
            &DOH_BOOTSTRAP_IPS,
        )?)?;
        let response: DnsJsonResponse = serde_json::from_slice(&body).map_err(|error| {
            Error::Dns(format!(
                "failed to decode DNS response for '{host}': {error}"
            ))
        })?;

        if response.status != DNS_STATUS_NOERROR {
            return Err(Error::Dns(format!(
                "DNS resolver returned status {} for '{host}'",
                response.status
            )));
        }

        let (addrs, cname, ttl) = parse_dns_answers(&response);
        if !addrs.is_empty() {
            return Ok((addrs, ttl));
        }

        if let Some(cname) = cname {
            return self.resolve_host_v4(&cname, depth + 1);
        }

        Err(Error::Dns(format!(
            "DNS lookup for '{host}' returned no IPv4 addresses"
        )))
    }
}

fn normalize_dns_name(name: &str) -> &str {
    name.trim_end_matches('.')
}

fn ttl_from_answers(answers: &[DnsAnswer]) -> Duration {
    let ttl_secs = answers
        .iter()
        .filter_map(|answer| answer.ttl)
        .min()
        .map(u64::from)
        .unwrap_or(DNS_CACHE_TTL_FLOOR_SECS)
        .clamp(DNS_CACHE_TTL_FLOOR_SECS, DNS_CACHE_TTL_CAP_SECS);
    Duration::from_secs(ttl_secs)
}

fn parse_dns_answers(response: &DnsJsonResponse) -> (Vec<Ipv4Addr>, Option<String>, Duration) {
    let mut addrs = Vec::new();
    let mut cname = None;

    for answer in &response.answers {
        match answer.record_type {
            DNS_RECORD_A => {
                if let Ok(ip) = answer.data.parse::<Ipv4Addr>() {
                    addrs.push(ip);
                }
            }
            DNS_RECORD_CNAME if cname.is_none() => {
                cname = Some(normalize_dns_name(&answer.data).to_string());
            }
            _ => {}
        }
    }

    (addrs, cname, ttl_from_answers(&response.answers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dns_answers_extracts_ipv4_records_and_ttl() {
        let response: DnsJsonResponse = serde_json::from_str(
            r#"{
                "Status": 0,
                "Answer": [
                    { "name": "api.coinbase.com.", "type": 5, "TTL": 300, "data": "edge.coinbase.com." },
                    { "name": "edge.coinbase.com.", "type": 1, "TTL": 120, "data": "104.16.1.10" },
                    { "name": "edge.coinbase.com.", "type": 1, "TTL": 60, "data": "104.16.2.10" }
                ]
            }"#,
        )
        .expect("decode DNS payload");

        let (addrs, cname, ttl) = parse_dns_answers(&response);
        assert_eq!(
            addrs,
            vec![Ipv4Addr::new(104, 16, 1, 10), Ipv4Addr::new(104, 16, 2, 10)]
        );
        assert_eq!(cname.as_deref(), Some("edge.coinbase.com"));
        assert_eq!(ttl, Duration::from_secs(60));
    }
}
