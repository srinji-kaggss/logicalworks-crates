//! Mutual-TLS end to end: a private CA, a server that can require client
//! certificates, and the shipped blocking client on the other side of the
//! handshake.
//!
//! The server below is a real `rustls` server built from a throwaway PKI
//! generated at test time (a dev-only edge, never linked into a shipped
//! artifact); the client is always the shipped `lgwks_std::http` API. One
//! seed varies the served body the handshake then protects, so a failure
//! names the seed and the same seed replays the same bytes. The TLS material
//! itself is fixed: one seed names one PKI, so trust is not a seeded
//! property. The PKI bytes come from the shared support module, the same
//! file the `http` unit seam includes, so both harnesses prove one PKI.
#![cfg(feature = "http")]
#![forbid(unsafe_code)]

use crate::pki;
use crate::rng;

#[cfg(feature = "http")]
mod sim {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    use lgwks_std::http::{Client, Error, FailureKind, FailureStage, Options, TlsOptions};

    use crate::sim_http_tls::pki::{TestPkiPems, test_pki_pems};
    use crate::sim_http_tls::rng::Rng;

    /// Seeds exercised per family; each draws one served body.
    const SEEDS: u64 = 8;

    /// The seed naming the test PKI: one seed, one PKI, replayable. The
    /// served bodies stay seeded per test; the PKI does not vary with them.
    /// The unit seam proves the same seed, so both harnesses prove one PKI.
    const TLS_PKI_SEED: u64 = 0x544C_5321;

    /// The PKI every test in this module proves against, generated once per
    /// process: RSA key generation dominates the cost, and every test needs
    /// the same fixed PKI.
    static TLS_PKI: std::sync::OnceLock<Result<TestPkiPems, String>> = std::sync::OnceLock::new();

    /// Borrow the shared PEM bundle, generating it on first use.
    fn shared_pems() -> Result<TestPkiPems, String> {
        TLS_PKI.get_or_init(|| test_pki_pems(TLS_PKI_SEED)).clone()
    }

    /// One served body, drawn from `seed` alone.
    fn tls_body(seed: u64) -> Result<Vec<u8>, String> {
        let mut rng = Rng::new(seed);
        let len = [0_usize, 1, 64, 1024][rng.below(4)];
        let byte = u8::try_from(rng.below(26))
            .map_err(|error| format!("the body draw does not fit a byte: {error:?}"))?;
        Ok(vec![b'B'.wrapping_add(byte); len])
    }

    /// Parse every certificate in `pem`.
    fn parse_certs(pem: &[u8]) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
        use rustls::pki_types::pem::PemObject;
        let mut certs = Vec::new();
        for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(pem) {
            certs.push(certificate.map_err(|error| error.to_string())?);
        }
        if certs.is_empty() {
            Err("test PKI holds no certificate".to_owned())
        } else {
            Ok(certs)
        }
    }

    /// Parse the first private key in `pem`, keeping its encoding.
    fn parse_key(pem: &[u8]) -> Result<rustls::pki_types::PrivateKeyDer<'static>, String> {
        use rustls::pki_types::pem::PemObject;
        if let Some(key) = rustls::pki_types::PrivateKeyDer::pem_slice_iter(pem).next() {
            return key.map_err(|error| error.to_string());
        }
        Err("test PKI holds no private key".to_owned())
    }

    /// Which server key form the test server presents: both are admitted.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ServerKeyForm {
        /// The PKCS#8 (`PRIVATE KEY`) form.
        Pkcs8,
        /// The SEC1 (`EC PRIVATE KEY`) form, with explicit parameters.
        Sec1,
    }

    /// The server key PEM in the form the test names.
    fn server_key_pem(pems: &TestPkiPems, form: ServerKeyForm) -> Vec<u8> {
        match form {
            ServerKeyForm::Pkcs8 => pems.server_key_pkcs8.clone(),
            ServerKeyForm::Sec1 => pems.server_key_sec1.clone(),
        }
    }

    /// What the test server observed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Served {
        /// The handshake completed; whether the client proved an identity.
        Completed {
            /// The client presented a certificate chain.
            had_client_cert: bool,
        },
        /// The handshake never completed.
        HandshakeFailed,
    }

    /// Serve one connection on `listener` with `body`, requiring a client
    /// certificate when asked, and report what the handshake decided.
    ///
    /// The server config is parsed from the same PEM blocks the client
    /// configures: the handshake proves one artifact on both sides. The
    /// server key arrives in the form the test names, so every admitted
    /// form serves a real handshake.
    fn serve_one_tls(
        listener: &TcpListener,
        body: Vec<u8>,
        require_client_auth: bool,
        pems: &TestPkiPems,
        server_key: ServerKeyForm,
    ) -> Result<Served, String> {
        let (mut sock, _) = listener.accept().map_err(|error| error.to_string())?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        for certificate in parse_certs(&pems.ca)? {
            roots.add(certificate).map_err(|error| error.to_string())?;
        }
        let verifier = if require_client_auth {
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|error| error.to_string())?;
            Some(verifier)
        } else {
            None
        };
        let server_certs = parse_certs(&pems.server)?;
        let server_key = parse_key(&server_key_pem(pems, server_key))?;
        let configured = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(rustls::ALL_VERSIONS)
            .map_err(|error| error.to_string())?;
        let config = match verifier {
            Some(verifier) => configured
                .with_client_cert_verifier(verifier)
                .with_single_cert(server_certs, server_key)
                .map_err(|error| error.to_string())?,
            None => configured
                .with_no_client_auth()
                .with_single_cert(server_certs, server_key)
                .map_err(|error| error.to_string())?,
        };
        let mut connection =
            rustls::ServerConnection::new(Arc::new(config)).map_err(|error| error.to_string())?;
        let open = {
            let mut stream = rustls::Stream::new(&mut connection, &mut sock);
            let mut request = [0_u8; 1024];
            let mut head = Vec::new();
            loop {
                match stream.read(&mut request) {
                    Ok(0) => break false,
                    Ok(read) => {
                        head.extend_from_slice(&request[..read]);
                        if head.windows(4).any(|window| window == b"\r\n\r\n") {
                            break true;
                        }
                    }
                    Err(_) => break false,
                }
            }
        };
        if !open {
            return Ok(Served::HandshakeFailed);
        }
        let had_client_cert = connection
            .peer_certificates()
            .is_some_and(|chain| !chain.is_empty());
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        {
            let mut stream = rustls::Stream::new(&mut connection, &mut sock);
            stream
                .write_all(reply.as_bytes())
                .map_err(|error| error.to_string())?;
            stream.write_all(&body).map_err(|error| error.to_string())?;
            stream.flush().map_err(|error| error.to_string())?;
        }
        Ok(Served::Completed { had_client_cert })
    }

    /// Run one test case: serve `body`, run the client, report both sides.
    ///
    /// The server presents its key in the form the test names, so the
    /// handshake itself proves that form.
    fn run_case(
        body: Vec<u8>,
        require_client_auth: bool,
        options: Options,
        pems: &TestPkiPems,
        server_key: ServerKeyForm,
    ) -> Result<(Served, Result<lgwks_std::http::Response, Error>), Box<dyn std::error::Error>>
    {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let pems = pems.clone();
        let server = std::thread::Builder::new()
            .name("lgwks-tls-server".to_owned())
            .spawn(move || serve_one_tls(&listener, body, require_client_auth, &pems, server_key))
            .map_err(|error| format!("the TLS server thread refused to start: {error}"))?;
        let client = Client::new(options)?;
        let answer = client.get_with(&format!("https://127.0.0.1:{port}/"), client.options());
        let served = server
            .join()
            .map_err(|payload| {
                let message = match payload.downcast_ref::<&str>() {
                    Some(text) => (*text).to_owned(),
                    None => match payload.downcast_ref::<String>() {
                        Some(text) => text.clone(),
                        None => "a payload that is not text".to_owned(),
                    },
                };
                format!("the TLS server thread panicked: {message}")
            })?
            .map_err(|refusal| format!("the TLS server refused: {refusal}"))?;
        Ok((served, answer))
    }

    /// A TLS refusal names the TLS stage and the transport class.
    fn assert_tls_failure(answer: Result<lgwks_std::http::Response, Error>, what: &str) {
        assert!(
            matches!(
                answer,
                Err(Error::Failure {
                    stage: FailureStage::Tls,
                    kind: FailureKind::Transport,
                    ..
                })
            ),
            "{what} must fail at the TLS stage, got {answer:?}"
        );
    }

    /// The handshake completed with the client authenticated, or the test
    /// names the exchange it was proving when it did not.
    fn assert_completed(served: Served, what: &str) {
        assert_eq!(
            served,
            Served::Completed {
                had_client_cert: true
            },
            "{what}"
        );
    }

    /// Run one mutual-TLS case and assert the served body arrives whole.
    ///
    /// The two success paths (every seed, every key-form combination) share
    /// this exchange so their assertions cannot drift apart: one function,
    /// one reading of the reply.
    fn assert_mutual_tls(
        body: Vec<u8>,
        options: Options,
        pems: &TestPkiPems,
        server_key: ServerKeyForm,
        what: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (served, answer) = run_case(body.clone(), true, options, pems, server_key)?;
        assert_completed(
            served,
            &format!("{what}: the server must see the client's certificate"),
        );
        let response = answer.map_err(|error| format!("{what}: mutual TLS failed: {error:?}"))?;
        assert_eq!(response.status, 200, "{what}: the exchange answers");
        assert_eq!(
            response.body(),
            body.as_slice(),
            "{what}: the served body arrives whole"
        );
        Ok(())
    }

    /// A client with the private CA and a matching identity completes mutual
    /// TLS and reads the served body, on every seed.
    #[test]
    fn private_ca_and_mutual_auth_succeed() -> Result<(), Box<dyn std::error::Error>> {
        let pems = shared_pems().map_err(|refusal| format!("the test PKI failed: {refusal}"))?;
        for seed in 0..SEEDS {
            let body = tls_body(seed).map_err(|refusal| format!("seed {seed}: {refusal}"))?;
            let options = Options::default().tls(
                TlsOptions::default()
                    .with_roots_pem(pems.ca.clone())
                    .with_client_chain_pem(pems.client.clone())
                    .with_client_key_pem(pems.client_key.clone()),
            );
            assert_mutual_tls(
                body,
                options,
                &pems,
                ServerKeyForm::Pkcs8,
                &format!("seed {seed}"),
            )?;
        }
        // The body is a pure function of the seed: the same seed replays.
        let first: Vec<Vec<u8>> = (0..SEEDS)
            .map(tls_body)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|refusal| format!("the replay draw failed: {refusal}"))?;
        let second: Vec<Vec<u8>> = (0..SEEDS)
            .map(tls_body)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|refusal| format!("the replay draw failed: {refusal}"))?;
        assert_eq!(
            first, second,
            "the same seed must derive the same body on every replay"
        );
        Ok(())
    }

    /// No identity against a server that requires one fails the handshake.
    #[test]
    fn no_identity_against_mtls_server_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let pems = shared_pems().map_err(|refusal| format!("the test PKI failed: {refusal}"))?;
        let options = Options::default().tls(TlsOptions::default().with_roots_pem(pems.ca.clone()));
        let (served, answer) = run_case(
            b"unread".to_vec(),
            true,
            options,
            &pems,
            ServerKeyForm::Pkcs8,
        )?;
        assert_eq!(
            served,
            Served::HandshakeFailed,
            "the server must reject the unauthenticated peer"
        );
        assert_tls_failure(answer, "no identity against mutual TLS");
        Ok(())
    }

    /// The system roots do not trust the private CA.
    #[test]
    fn system_roots_do_not_trust_the_private_ca() -> Result<(), Box<dyn std::error::Error>> {
        let pems = shared_pems().map_err(|refusal| format!("the test PKI failed: {refusal}"))?;
        let (served, answer) = run_case(
            b"unread".to_vec(),
            false,
            Options::default(),
            &pems,
            ServerKeyForm::Pkcs8,
        )?;
        assert_eq!(
            served,
            Served::HandshakeFailed,
            "the handshake must fail before any request is served"
        );
        assert_tls_failure(answer, "a private CA under the system roots");
        Ok(())
    }

    /// Trusting the leaf instead of the CA does not complete the chain.
    #[test]
    fn wrong_roots_are_refused() -> Result<(), Box<dyn std::error::Error>> {
        let pems = shared_pems().map_err(|refusal| format!("the test PKI failed: {refusal}"))?;
        let options =
            Options::default().tls(TlsOptions::default().with_roots_pem(pems.server.clone()));
        let (served, answer) = run_case(
            b"unread".to_vec(),
            false,
            options,
            &pems,
            ServerKeyForm::Pkcs8,
        )?;
        assert_eq!(
            served,
            Served::HandshakeFailed,
            "the handshake must fail before any request is served"
        );
        assert_tls_failure(answer, "a leaf trusted as a root");
        Ok(())
    }

    /// Every admitted key-form combination completes mutual TLS: the server
    /// presents each EC form while the client presents each RSA form, so the
    /// handshake itself proves the forms the unit seam only builds.
    #[test]
    fn admitted_forms_complete_mutual_tls() -> Result<(), Box<dyn std::error::Error>> {
        let pems = shared_pems().map_err(|refusal| format!("the test PKI failed: {refusal}"))?;
        for (server_key, pkcs1, what) in [
            (
                ServerKeyForm::Pkcs8,
                false,
                "a PKCS#8 server key with a PKCS#8 client key",
            ),
            (
                ServerKeyForm::Sec1,
                false,
                "a SEC1 server key with a PKCS#8 client key",
            ),
            (
                ServerKeyForm::Pkcs8,
                true,
                "a PKCS#8 server key with a PKCS#1 client key",
            ),
            (
                ServerKeyForm::Sec1,
                true,
                "a SEC1 server key with a PKCS#1 client key",
            ),
        ] {
            let client_key = if pkcs1 {
                pems.client_key_pkcs1.clone()
            } else {
                pems.client_key.clone()
            };
            let options = Options::default().tls(
                TlsOptions::default()
                    .with_roots_pem(pems.ca.clone())
                    .with_client_chain_pem(pems.client.clone())
                    .with_client_key_pem(client_key),
            );
            assert_mutual_tls(b"formed".to_vec(), options, &pems, server_key, what)?;
        }
        Ok(())
    }

    /// Identity material that cannot match builds no client: an unrelated key,
    /// and the Ed25519 pair the loader refuses by policy. No server runs here;
    /// the refusal lands before any socket opens.
    #[test]
    fn refused_identity_material_builds_no_client() -> Result<(), Box<dyn std::error::Error>> {
        let pems = shared_pems().map_err(|refusal| format!("the test PKI failed: {refusal}"))?;
        for (chain, key, what) in [
            (
                pems.client.clone(),
                pems.wrong_key.clone(),
                "an unrelated key",
            ),
            (pems.ed_cert.clone(), pems.ed_key.clone(), "Ed25519"),
        ] {
            let built = Client::new(
                Options::default().tls(
                    TlsOptions::default()
                        .with_roots_pem(pems.ca.clone())
                        .with_client_chain_pem(chain)
                        .with_client_key_pem(key),
                ),
            );
            assert!(
                matches!(
                    built,
                    Err(Error::Failure {
                        kind: FailureKind::InvalidRequest,
                        stage: FailureStage::Tls,
                        ..
                    })
                ),
                "{what} never reaches the wire: got {built:?}"
            );
        }
        Ok(())
    }
}
