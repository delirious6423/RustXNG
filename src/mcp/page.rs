// SPDX-License-Identifier: AGPL-3.0-or-later
//! Page fetching for `read_page`, safe against server-side request forgery.
//!
//! Search results come from third parties, so a returned URL may point at a
//! private service, a hostname may resolve to one, and a public page may
//! redirect to one. This fetcher therefore:
//!
//! - refuses literal addresses that are not globally routable, IPv4 and IPv6,
//! - resolves hostnames itself and connects only to public addresses (the
//!   checked answers are the ones used, so DNS rebinding cannot swap them),
//! - checks every redirect hop the same way,
//! - stops reading a body after `MAX_BODY_BYTES` decoded bytes, and
//! - parses at most `MAX_CONCURRENT` pages at a time, off the async runtime.
//!
//! It does not use the shared response cache, whose entries may have been
//! fetched without these checks. Extracted text is kept per session instead,
//! so reading a page slice by slice fetches it once.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Decoded body bytes read from one page at most.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Pages fetched and parsed at the same time at most.
const MAX_CONCURRENT: usize = 4;
/// Extracted pages kept per session for paging.
const REMEMBERED_PAGES: usize = 8;
/// Time for connecting, redirects and the whole body.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_REDIRECTS: usize = 5;

/// Whether an address is globally routable (not loopback, private, link
/// local, shared, multicast, documentation or otherwise reserved).
pub fn is_global(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_global_v4(address),
        IpAddr::V6(address) => is_global_v6(address),
    }
}

fn is_global_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..128).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (18..20).contains(&b))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn is_global_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return is_global_v4(v4);
    }
    let segments = address.segments();
    let embedded_v4 = |high: u16, low: u16| {
        Ipv4Addr::new((high >> 8) as u8, high as u8, (low >> 8) as u8, low as u8)
    };
    match segments[0] {
        // NAT64 (64:ff9b::/96) reaches the embedded IPv4 address.
        0x0064 if segments[1] == 0xff9b => is_global_v4(embedded_v4(segments[6], segments[7])),
        // 6to4 (2002::/16) reaches the embedded IPv4 address.
        0x2002 => is_global_v4(embedded_v4(segments[1], segments[2])),
        // Teredo, benchmarking and other IETF protocol assignments; documentation.
        0x2001 => segments[1] >= 0x0200 && segments[1] != 0x0db8,
        // Only global unicast (2000::/3) is public; this also excludes
        // ::/8 (loopback, unspecified, IPv4-compatible), fc00::/7, fe80::/10
        // and ff00::/8.
        first => (0x2000..0x4000).contains(&first) && first != 0x3fff,
    }
}

/// Whether a URL may be fetched: http(s) and not a literal non-global address.
/// Hostnames are checked when they are resolved.
pub fn is_fetchable(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(address)) => is_global_v4(address),
        Some(url::Host::Ipv6(address)) => is_global_v6(address),
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain != "localhost" && !domain.ends_with(".localhost")
        }
        None => false,
    }
}

/// Resolves with the system resolver and keeps only global addresses.
struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| is_global(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// Extracted pages as (URL, text), oldest first.
type Pages = VecDeque<(String, Arc<str>)>;

/// Fetches pages and keeps their extracted text for the session.
#[derive(Clone)]
pub struct PageReader {
    client: reqwest::Client,
    permits: Arc<tokio::sync::Semaphore>,
    pages: Arc<Mutex<Pages>>,
}

impl PageReader {
    /// A reader using the outgoing settings' TLS, HTTP/2 and proxy choices.
    /// `overrides` pin hostnames to addresses without checks; tests use it
    /// to serve pages from loopback.
    pub fn new(
        outgoing: &crate::settings::Outgoing,
        overrides: &[(String, SocketAddr)],
    ) -> Result<Self, String> {
        let redirects = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if is_fetchable(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("redirect to a non-public address")
            }
        });
        let mut builder = reqwest::Client::builder()
            .redirect(redirects)
            .dns_resolver(Arc::new(PublicResolver))
            .timeout(FETCH_TIMEOUT)
            .connect_timeout(Duration::from_secs(4))
            .danger_accept_invalid_certs(!outgoing.verify)
            .user_agent(crate::network::default_user_agent());
        if !outgoing.enable_http2 {
            builder = builder.http1_only();
        }
        // A configured proxy resolves names itself; literal addresses and
        // redirects are still checked here.
        for proxy in outgoing.proxies.values() {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|error| error.to_string())?);
        }
        for (host, address) in overrides {
            builder = builder.resolve(host, *address);
        }
        Ok(Self {
            client: builder.build().map_err(|error| error.to_string())?,
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT)),
            pages: Arc::new(Mutex::new(VecDeque::new())),
        })
    }

    /// The readable text of a page, up to `max_chars` characters (with a
    /// trailing '…' when there was more), or an explanation.
    pub async fn text(&self, url: &str, max_chars: usize) -> Result<Arc<str>, String> {
        if let Some(text) = self.remembered(url) {
            return Ok(text);
        }
        let parsed = reqwest::Url::parse(url).map_err(|_| "not a valid URL".to_string())?;
        if !is_fetchable(&parsed) {
            return Err("only public http(s) pages can be read".into());
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|error| error.to_string())?;
        let html = tokio::time::timeout(FETCH_TIMEOUT, self.fetch(parsed))
            .await
            .map_err(|_| "the page took too long to load".to_string())??;
        let text = tokio::task::spawn_blocking(move || {
            let document = scraper::Html::parse_document(&html);
            crate::content::main_text_limited(&document, max_chars)
        })
        .await
        .map_err(|error| error.to_string())?;
        if text.is_empty() {
            return Err("the page has no readable text".into());
        }
        let text: Arc<str> = text.into();
        self.remember(url, text.clone());
        Ok(text)
    }

    async fn fetch(&self, url: reqwest::Url) -> Result<String, String> {
        let mut response = self
            .client
            .get(url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml;q=0.9,text/plain;q=0.8",
            )
            .send()
            .await
            .map_err(|error| format!("the page could not be fetched: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("the page answered {}", response.status()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/html")
            .to_ascii_lowercase();
        if !(content_type.starts_with("text/") || content_type.contains("xhtml")) {
            return Err(format!("not a text page ({content_type})"));
        }
        // Read at most MAX_BODY_BYTES decoded bytes, whatever Content-Length says.
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("the page could not be read: {error}"))?
        {
            let room = MAX_BODY_BYTES - body.len();
            if chunk.len() >= room {
                body.extend_from_slice(&chunk[..room]);
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    fn remembered(&self, url: &str) -> Option<Arc<str>> {
        let pages = self
            .pages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pages
            .iter()
            .find(|(known, _)| known == url)
            .map(|(_, text)| text.clone())
    }

    fn remember(&self, url: &str, text: Arc<str>) {
        let mut pages = self
            .pages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pages.retain(|(known, _)| known != url);
        pages.push_back((url.to_string(), text));
        if pages.len() > REMEMBERED_PAGES {
            pages.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_global_addresses_are_public() {
        for private in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::127.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2001::1",
            "2001:db8::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "100::1",
        ] {
            assert!(!is_global(private.parse().unwrap()), "{private}");
        }
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.215.14",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::",
            "2606:4700::1111",
            "2a00:1450::1",
            "2001:4860::8888",
        ] {
            assert!(is_global(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn literal_hosts_and_schemes_are_checked() {
        for refused in [
            "http://127.0.0.1/",
            "http://[::ffff:127.0.0.1]:8080/",
            "http://[fe80::1]/",
            "http://localhost/",
            "http://a.localhost/",
            "http://LOCALHOST./",
            "file:///etc/passwd",
            "ftp://example.com/",
            "http://169.254.169.254/latest",
        ] {
            assert!(
                !is_fetchable(&reqwest::Url::parse(refused).unwrap()),
                "{refused}"
            );
        }
        for allowed in [
            "https://example.com/",
            "http://8.8.8.8/",
            "https://[2606:4700::1111]/",
        ] {
            assert!(
                is_fetchable(&reqwest::Url::parse(allowed).unwrap()),
                "{allowed}"
            );
        }
    }

    #[tokio::test]
    async fn names_resolving_to_private_addresses_are_refused() {
        let error = PublicResolver
            .resolve("localhost".parse().unwrap())
            .await
            .err()
            .expect("localhost must not resolve");
        assert!(error.to_string().contains("no public address"), "{error}");
    }
}
