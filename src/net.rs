//! Outbound HTTP setup shared by every ureq 2 request croft makes (#872):
//! which proxy a request goes through and which certificate authorities it
//! trusts, decided in one place so a download works wherever curl does.
//!
//! ureq on its own trusts only the Mozilla roots compiled into the binary and
//! ignores `HTTPS_PROXY`. Behind a TLS-inspecting gateway (Zscaler, Netskope
//! and the like) every certificate is re-signed by a CA that only the
//! platform store knows, so each download failed with "UnknownIssuer" while
//! curl, pip and cargo on the same machine worked.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use rustls::pki_types::CertificateDer;

/// An agent builder for a request to `url`: through the proxy the environment
/// names for its scheme unless `no_proxy` exempts the host, and trusting the
/// platform CA store (or `SSL_CERT_FILE` / `SSL_CERT_DIR` when set) on top of
/// the Mozilla roots. Callers add their own timeouts and redirect policy.
pub fn builder(url: &str) -> ureq::AgentBuilder {
    let builder = ureq::AgentBuilder::new().tls_config(tls_config());
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    match proxy_for(url, &env).and_then(|p| ureq::Proxy::new(p).ok()) {
        Some(proxy) => builder.proxy(proxy),
        None => builder,
    }
}

/// A GET of `url` on a [`builder`] agent with ureq's default timeouts.
pub fn get(url: &str) -> ureq::Request {
    builder(url).build().get(url)
}

/// Why a request failed, short enough for a status line: the HTTP status, or
/// the transport failure and its cause (DNS, refused, timeout, TLS), without
/// the URL ureq's own message opens with. A certificate nothing in the trust
/// store vouches for gets a pointer at the usual reason.
pub fn describe(err: &ureq::Error) -> String {
    let t = match err {
        ureq::Error::Status(code, _) => return format!("HTTP {code}"),
        ureq::Error::Transport(t) => t,
    };
    let mut why = t.kind().to_string();
    if let Some(message) = t.message() {
        why.push_str(&format!(": {message}"));
    }
    let source = std::error::Error::source(t);
    if let Some(source) = source {
        why.push_str(&format!(": {source}"));
    }
    let bad_cert = source
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .and_then(|e| e.get_ref())
        .and_then(|e| e.downcast_ref::<rustls::Error>())
        .is_some_and(|e| matches!(e, rustls::Error::InvalidCertificate(_)));
    if bad_cert {
        why.push_str(
            " (behind a TLS-inspecting proxy? add its CA to the system store or SSL_CERT_FILE)",
        );
    }
    why
}

/// The client TLS config every request shares, built on first use.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let native = rustls_native_certs::load_native_certs();
        for e in &native.errors {
            crate::lsp::log_file::log(&format!("net: reading the platform CA store: {e}"));
        }
        client_config(native.certs)
    });
    Arc::clone(config)
}

/// A client config trusting the Mozilla roots plus `platform`, the certificates
/// the OS store holds. The Mozilla roots stay so a machine whose store is
/// empty or unreadable still connects as it did before.
fn client_config(platform: Vec<CertificateDer<'static>>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    roots.add_parsable_certificates(platform);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports TLS 1.2 and 1.3")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// The proxy for `url`, read from `env` as curl reads it: `https_proxy` for
/// https and `http_proxy` for http (either case), then `all_proxy`. `None`
/// when none is set, and for a host `no_proxy` exempts.
fn proxy_for(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let keys = match url.scheme() {
        "https" => ["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"],
        "http" => ["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"],
        _ => return None,
    };
    let proxy = keys.iter().find_map(|k| env(k))?;
    let no_proxy = env("no_proxy")
        .or_else(|| env("NO_PROXY"))
        .unwrap_or_default();
    (!bypasses(&url.host()?, &no_proxy)).then_some(proxy)
}

/// Whether `host` skips the proxy: loopback always (no proxy can reach it on
/// the user's behalf), otherwise when a `no_proxy` entry covers it: `*`, the
/// host or a parent domain (with or without a leading `.` or `*.`), an IP, or
/// a CIDR range holding it. Names are not resolved, as in curl.
fn bypasses(host: &url::Host<&str>, no_proxy: &str) -> bool {
    let (name, ip): (&str, Option<IpAddr>) = match *host {
        url::Host::Domain(d) => (d, None),
        url::Host::Ipv4(a) => ("", Some(a.into())),
        url::Host::Ipv6(a) => ("", Some(a.into())),
    };
    if name == "localhost" || ip.is_some_and(|ip| ip.is_loopback()) {
        return true;
    }
    let entries = no_proxy
        .split(',')
        .map(|e| e.trim().trim_matches(['[', ']']));
    entries.filter(|e| !e.is_empty()).any(|entry| {
        if entry == "*" {
            return true;
        }
        match (ip, entry.split_once('/')) {
            (Some(ip), Some((net, bits))) => in_cidr(ip, net, bits),
            (Some(ip), None) => entry.parse::<IpAddr>().is_ok_and(|e| e == ip),
            (None, _) => {
                let domain = entry.trim_start_matches('*').trim_start_matches('.');
                let domain = domain.to_ascii_lowercase();
                let sub = |p: &str| p.ends_with('.');
                !domain.is_empty()
                    && (name == domain || name.strip_suffix(&*domain).is_some_and(sub))
            }
        }
    })
}

/// Whether `ip` lies in the range `net`/`bits`.
fn in_cidr(ip: IpAddr, net: &str, bits: &str) -> bool {
    let (Ok(net), Ok(bits)) = (net.parse::<IpAddr>(), bits.parse::<u32>()) else {
        return false;
    };
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::path::Path;

    /// The self-signed `localhost` certificate `croft web --tls` tests with.
    fn test_cert() -> Vec<CertificateDer<'static>> {
        use rustls::pki_types::pem::PemObject;
        let pem = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web/testdata/cert.pem");
        CertificateDer::pem_file_iter(pem)
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// An HTTPS server on loopback whose certificate only [`test_cert`]
    /// vouches for, standing in for a TLS-inspecting proxy whose CA is in
    /// the platform store but not among the Mozilla roots. Answers every
    /// request with `ok`; returns its base URL.
    pub(crate) fn https_server() -> String {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web/testdata");
        let config = crate::web::tls::load(&here.join("cert.pem"), &here.join("key.pem")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for tcp in listener.incoming().flatten() {
                let Ok(mut plain) = crate::web::tls::terminate(tcp, Arc::clone(&config)) else {
                    continue;
                };
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && plain.read(&mut b).unwrap_or(0) == 1 {
                        head.push(b[0]);
                    }
                    let reply =
                        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
                    let _ = plain.write_all(reply.as_bytes());
                });
            }
        });
        format!("https://127.0.0.1:{port}/")
    }

    #[test]
    fn a_ca_from_the_platform_store_is_trusted_alongside_the_mozilla_roots() {
        let url = https_server();
        let with_platform = ureq::AgentBuilder::new().tls_config(client_config(test_cert()));
        let body = with_platform
            .build()
            .get(&url)
            .call()
            .unwrap()
            .into_string();
        assert_eq!(body.unwrap(), "ok");
        // The Mozilla roots alone (ureq's own default) cannot place it, and
        // the reason names the certificate rather than vanishing.
        let without = ureq::AgentBuilder::new().tls_config(client_config(Vec::new()));
        let err = without.build().get(&url).call().unwrap_err();
        let why = describe(&err);
        assert!(why.contains("UnknownIssuer"), "{why}");
        assert!(why.contains("SSL_CERT_FILE"), "{why}");
        assert!(!why.contains("127.0.0.1"), "no URL in the reason: {why}");
    }

    #[test]
    fn the_proxy_follows_the_scheme_and_no_proxy() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
        };
        const NO_PROXY: &str = "internal.corp, .lan,*.svc.local,10.0.0.0/8,192.168.1.7,[fd00::1]";
        let corp = env(&[
            ("HTTPS_PROXY", "http://gw:8080"),
            ("http_proxy", "http://plain:3128"),
            ("NO_PROXY", NO_PROXY),
        ]);
        let proxy = |url: &str| proxy_for(url, &corp).unwrap_or_default();
        assert_eq!(proxy("https://github.com/x"), "http://gw:8080");
        assert_eq!(proxy("http://example.com/"), "http://plain:3128");
        for exempt in [
            "https://internal.corp/",
            "https://git.internal.corp/",
            "https://nas.lan/",
            "https://api.svc.local/",
            "https://10.20.30.40/",
            "https://192.168.1.7:8443/",
            "https://[fd00::1]/",
            "http://localhost:3000/",
            "http://127.0.0.1:8080/",
            "http://[::1]/",
        ] {
            assert_eq!(proxy(exempt), "", "{exempt}");
        }
        // A suffix or a range only matches on its boundary.
        for outside in [
            "https://notinternal.corp/",
            "https://192.168.1.8/",
            "https://11.0.0.1/",
        ] {
            assert_eq!(proxy(outside), "http://gw:8080", "{outside}");
        }
        // all_proxy is the fallback, `*` exempts every host, and with nothing
        // set there is no proxy.
        let all = env(&[("ALL_PROXY", "http://all:1")]);
        assert_eq!(
            proxy_for("https://a.b/", &all).as_deref(),
            Some("http://all:1")
        );
        let star = env(&[("https_proxy", "http://gw:1"), ("no_proxy", "*")]);
        assert_eq!(proxy_for("https://a.b/", &star), None);
        assert_eq!(proxy_for("https://a.b/", &env(&[])), None);
    }
}
