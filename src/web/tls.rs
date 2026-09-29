//! `croft web --tls CERT KEY` (#343): TLS in front of the WebSocket bridge.
//!
//! rustls with the ring provider is already in the dependency graph (ureq's
//! HTTPS), so this adds no crate. Each accepted connection is terminated
//! here and handed on as a plain socket pair, so the upgrade and the bridge
//! in `web` are the same code with or without TLS.
//!
//! No certificate is generated in the binary: bring one from `mkcert`, or
//! front `croft web` with Tailscale Serve or Caddy (docs/WEB.md).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};

/// The server config for a PEM certificate chain and its private key.
pub fn load(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>> {
    let certs = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("reading {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parsing {}", cert.display()))?;
    anyhow::ensure!(!certs.is_empty(), "{} holds no certificate", cert.display());
    let key = PrivateKeyDer::from_pem_file(key)
        .with_context(|| format!("reading a private key from {}", key.display()))?;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .context("TLS protocol versions")?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("the certificate and key do not form a usable pair")?;
    Ok(Arc::new(config))
}

/// Terminate TLS on `tcp` and return the plaintext side as a socket.
///
/// Two threads pump the connection, one per direction, sharing the rustls
/// state. A record is written to `tcp` while the lock is held: TLS records
/// carry sequence numbers, so handshake output from one thread and
/// application data from the other must reach the wire in the order rustls
/// produced them.
pub fn terminate(tcp: TcpStream, config: Arc<ServerConfig>) -> std::io::Result<UnixStream> {
    let tls = ServerConnection::new(config).map_err(std::io::Error::other)?;
    let tls = Arc::new(Mutex::new(tls));
    let (ours, theirs) = UnixStream::pair()?;

    // Network to plaintext.
    {
        let tls = Arc::clone(&tls);
        let mut from_net = tcp.try_clone()?;
        let to_net = tcp.try_clone()?;
        let mut to_plain = ours.try_clone()?;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            let mut plain = vec![0u8; 16 * 1024];
            'pump: while let Ok(n) = from_net.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut received = Vec::new();
                let mut ended = false;
                {
                    let mut tls = tls.lock().unwrap();
                    let mut input = &buf[..n];
                    while !input.is_empty() {
                        if tls.read_tls(&mut input).is_err() {
                            break 'pump;
                        }
                        let processed = tls.process_new_packets();
                        // An alert for a bad handshake is still owed to the
                        // peer before the connection ends.
                        if flush(&mut tls, &to_net).is_err() || processed.is_err() {
                            break 'pump;
                        }
                    }
                    loop {
                        match tls.reader().read(&mut plain) {
                            Ok(0) => {
                                ended = true;
                                break;
                            }
                            Ok(k) => received.extend_from_slice(&plain[..k]),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => {
                                ended = true;
                                break;
                            }
                        }
                    }
                }
                if to_plain.write_all(&received).is_err() || ended {
                    break;
                }
            }
            let _ = to_plain.shutdown(std::net::Shutdown::Write);
        });
    }

    // Plaintext to network.
    std::thread::spawn(move || {
        let mut from_plain = ours;
        let mut buf = vec![0u8; 16 * 1024];
        while let Ok(n) = from_plain.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut tls = tls.lock().unwrap();
            if tls.writer().write_all(&buf[..n]).is_err() || flush(&mut tls, &tcp).is_err() {
                break;
            }
        }
        let mut tls = tls.lock().unwrap();
        tls.send_close_notify();
        let _ = flush(&mut tls, &tcp);
        let _ = tcp.shutdown(std::net::Shutdown::Both);
    });

    Ok(theirs)
}

/// Write every pending TLS record to the network.
fn flush(tls: &mut ServerConnection, mut net: &TcpStream) -> std::io::Result<()> {
    while tls.wants_write() {
        tls.write_tls(&mut net)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write far larger than one TLS record (a big paste, a burst of
    /// screen output) arrives whole in both directions.
    #[test]
    fn a_large_write_passes_through_whole() {
        use rustls::pki_types::pem::PemObject;
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web/testdata");
        let config = load(&here.join("cert.pem"), &here.join("key.pem")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            terminate(tcp, config).unwrap()
        });

        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(here.join("cert.pem")).unwrap() {
            roots.add(cert.unwrap()).unwrap();
        }
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let conn = rustls::ClientConnection::new(Arc::new(client), "localhost".try_into().unwrap())
            .unwrap();
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut tls = rustls::StreamOwned::new(conn, tcp);

        let big: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
        tls.write_all(&big).unwrap();
        tls.flush().unwrap();
        let mut plain = server.join().unwrap();
        plain
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut got = vec![0u8; big.len()];
        plain.read_exact(&mut got).unwrap();
        assert!(got == big, "the client's bytes arrived changed");

        // And back: the plaintext side's write reaches the client.
        plain.write_all(&big).unwrap();
        let mut back = vec![0u8; big.len()];
        tls.read_exact(&mut back).unwrap();
        assert!(back == big, "the server's bytes arrived changed");
    }

    #[test]
    fn a_missing_certificate_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load(&dir.path().join("cert.pem"), &dir.path().join("key.pem")).unwrap_err();
        assert!(format!("{err:#}").contains("cert.pem"), "{err:#}");
    }

    #[test]
    fn a_file_with_no_certificate_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        std::fs::write(&cert, "not a certificate\n").unwrap();
        let err = load(&cert, &dir.path().join("key.pem")).unwrap_err();
        assert!(format!("{err:#}").contains("no certificate"), "{err:#}");
    }
}
