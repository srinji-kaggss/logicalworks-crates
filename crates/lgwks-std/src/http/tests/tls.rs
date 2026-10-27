//! Private-CA roots and client identity, proved against a runtime test PKI.
//!
//! The PKI is generated when the test runs from the fixed seed below: a CA,
//! a server certificate with `localhost`/`127.0.0.1` SANs in three key forms
//! (SEC1, PKCS#8 EC, PKCS#1 and PKCS#8 RSA), a client certificate with its
//! key, an unrelated key for the mismatch proof, and an Ed25519 pair for the
//! refusal proof. Nothing here is trusted by anyone; the point is shape, not
//! authority. Committing private keys is refused by the repository's own gate,
//! so no fixture is read from disk and every byte below is generated
//! in-process. The generation itself lives in the shared support module,
//! included here by path: a unit test cannot declare `tests/` as a module,
//! and a second copy of the generator would be a second PKI the two
//! harnesses could silently disagree on.

use super::*;
extern crate self as lgwks_std;

/// The shared throwaway-PKI module, included by path rather than copied.
/// The `extern crate self` alias above is what makes its `lgwks_std::`
/// paths resolve inside this crate.
#[path = "../../../tests/support/pki.rs"]
mod pki;

use pki::{TestPkiPems, test_pki_pems};
use std::sync::OnceLock;

/// The seed naming the unit-test PKI: one seed, one PKI, replayable.
const TEST_PKI_SEED: u64 = 0x544C_5321;

/// The PKI every test in this module proves against, generated once.
///
/// RSA key generation dominates the cost and every test needs the same fixed
/// PKI, so the first test to arrive generates it and the rest share it.
static TEST_PKI: OnceLock<Result<TestPkiPems, String>> = OnceLock::new();

/// Borrow the shared PEM bundle, generating it on first use.
fn fixtures() -> Result<TestPkiPems, String> {
    TEST_PKI
        .get_or_init(|| test_pki_pems(TEST_PKI_SEED))
        .clone()
}

/// A TLS refusal names its stage and class, never the file that caused it.
fn assert_tls_refusal(result: Result<Client, Error>, what: &str) {
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Tls,
                kind: FailureKind::InvalidRequest,
                ..
            })
        ),
        "{what} must be a typed TLS refusal, got {result:?}"
    );
}

/// Valid roots build a client.
#[test]
fn valid_roots_build_a_client() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    let options = Options::default().tls(TlsOptions::default().with_roots_pem(pems.ca));
    let client = Client::new(options)?;
    assert_eq!(
        client.options().max_body_bytes,
        DEFAULT_MAX_BODY_BYTES,
        "a TLS client is an ordinary client otherwise"
    );
    Ok(())
}

/// Garbage, empty, and key-mixed roots are refused before any socket opens.
#[test]
fn unparseable_roots_are_refused() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    for (pem, what) in [
        (b"not a PEM block".to_vec(), "garbage"),
        (Vec::new(), "empty"),
        (pems.client_key, "a private key as roots"),
    ] {
        assert_tls_refusal(
            Client::new(Options::default().tls(TlsOptions::default().with_roots_pem(pem))),
            what,
        );
    }
    Ok(())
}

/// A chain without a key, or a key without a chain, is a caller bug.
#[test]
fn half_an_identity_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    assert_tls_refusal(
        Client::new(
            Options::default().tls(TlsOptions::default().with_client_chain_pem(pems.client)),
        ),
        "a chain without a key",
    );
    assert_tls_refusal(
        Client::new(
            Options::default().tls(TlsOptions::default().with_client_key_pem(pems.client_key)),
        ),
        "a key without a chain",
    );
    Ok(())
}

/// A key that does not match the certificate is refused, never panicked on.
#[test]
fn mismatched_identity_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    assert_tls_refusal(
        Client::new(
            Options::default().tls(
                TlsOptions::default()
                    .with_client_chain_pem(pems.client.clone())
                    .with_client_key_pem(pems.wrong_key),
            ),
        ),
        "an unrelated key",
    );
    assert_tls_refusal(
        Client::new(
            Options::default().tls(
                TlsOptions::default()
                    .with_client_chain_pem(pems.client)
                    .with_client_key_pem(pems.server_key_sec1),
            ),
        ),
        "the server's key with the client's certificate",
    );
    Ok(())
}

/// Every admitted key form builds when it matches its certificate.
#[test]
fn every_admitted_key_form_builds() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    for (chain, key, what) in [
        (pems.client.clone(), pems.client_key, "PKCS#8 RSA"),
        (pems.client, pems.client_key_pkcs1, "PKCS#1 RSA"),
        (pems.server.clone(), pems.server_key_sec1, "SEC1 EC"),
        (pems.server, pems.server_key_pkcs8, "PKCS#8 EC"),
    ] {
        let options = Options::default().tls(
            TlsOptions::default()
                .with_client_chain_pem(chain)
                .with_client_key_pem(key),
        );
        assert!(Client::new(options).is_ok(), "{what} identity must build");
    }
    Ok(())
}

/// Ed25519 identity material is refused with a typed error.
#[test]
fn ed25519_identity_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    assert_tls_refusal(
        Client::new(
            Options::default().tls(
                TlsOptions::default()
                    .with_client_chain_pem(pems.ed_cert)
                    .with_client_key_pem(pems.ed_key),
            ),
        ),
        "Ed25519",
    );
    Ok(())
}

/// Every truncation of a root, chain, or key file is refused, unless the
/// cut lands past the last byte that matters: a PEM block without its
/// trailing newline still parses.
#[test]
fn every_truncation_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    let whole = [
        (pems.ca, "roots", false),
        (pems.client, "chain", false),
        (pems.client_key, "key", true),
    ];
    for (fixture, role, is_key) in whole.iter().map(|slot| (&slot.0, slot.1, slot.2)) {
        for end in 0..fixture.len() {
            let prefix = fixture[..end].to_vec();
            let trailing_newline = fixture
                .strip_suffix(b"\n")
                .is_some_and(|stripped| stripped == prefix.as_slice());
            let complete = end == fixture.len() || trailing_newline;
            let built = if is_key {
                Client::new(
                    Options::default().tls(
                        TlsOptions::default()
                            .with_client_chain_pem(whole[1].0.clone())
                            .with_client_key_pem(prefix),
                    ),
                )
            } else if role == "roots" {
                Client::new(Options::default().tls(TlsOptions::default().with_roots_pem(prefix)))
            } else {
                Client::new(
                    Options::default().tls(
                        TlsOptions::default()
                            .with_client_chain_pem(prefix)
                            .with_client_key_pem(whole[2].0.clone()),
                    ),
                )
            };
            assert!(
                built.is_ok() == complete,
                "a {end}-byte prefix of the {role} builds exactly when it is whole"
            );
        }
    }
    Ok(())
}

/// Debugging a TLS configuration never renders key or certificate bytes.
#[test]
fn debug_rendering_redacts_material() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    let options = TlsOptions::default()
        .with_roots_pem(pems.ca)
        .with_client_chain_pem(pems.client)
        .with_client_key_pem(pems.client_key);
    let rendered = format!("{options:?}");
    assert!(
        !rendered.contains("PRIVATE"),
        "no key text reaches the debug rendering: {rendered}"
    );
    assert!(
        !rendered.contains("BEGIN CERTIFICATE"),
        "no certificate text reaches the debug rendering: {rendered}"
    );
    Ok(())
}

/// A TLS-configured client still reaches plain HTTP loopback.
#[test]
fn tls_material_does_not_break_plain_http() -> Result<(), Box<dyn std::error::Error>> {
    let pems = fixtures().map_err(|refusal| format!("the unit PKI failed: {refusal}"))?;
    let (port, server) = serve(vec![("200 OK", "plain".to_owned())])?;
    let options = Options::default()
        .tls(
            TlsOptions::default()
                .with_roots_pem(pems.ca)
                .with_client_chain_pem(pems.client)
                .with_client_key_pem(pems.client_key),
        )
        .max_body_bytes(64);
    let client = Client::new(options)?;
    let response = client.get(&format!("http://127.0.0.1:{port}/"))?;
    assert_eq!(
        response.status, 200,
        "plain HTTP answers through a TLS client"
    );
    assert_eq!(response.text()?, "plain", "and the body is whole");
    join_server(server)?;
    Ok(())
}
