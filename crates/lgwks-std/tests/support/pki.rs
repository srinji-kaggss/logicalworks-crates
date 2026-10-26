//! Throwaway test PKI, generated at test time: nothing here is trusted by anyone.
//!
//! Committing private keys is refused by the repository's own gate, so every
//! key below is generated when the test runs and lives only in its process.
//! RSA comes from the `rsa` crate fed by the estate's seeded stream (one
//! seed, one PKI, replayable); EC and Ed25519 keys and every certificate come
//! from `rcgen`. PEM blocks are wrapped here with the estate's own base64, so
//! no PEM serializer enters the tree.
//!
//! This module is shared source: the loopback sims include it from `tests/`,
//! and the `http` unit seam includes the same file with `#[path]` (a unit
//! test cannot declare `tests/` as a module). It therefore names the estate
//! through `lgwks_std::` paths, which resolve to the real crate from an
//! integration binary and to the unit seam's `extern crate self` alias from
//! the library. One file, one PKI, both harnesses: a second copy here would
//! be a second PKI the two harnesses could silently disagree on.
//!
//! Rustls backs the TLS side of the end-to-end proof with the same bytes in
//! DER form; where a PEM-only shape is needed (SEC1, PKCS#1) the DER is
//! re-wrapped here rather than re-encoded by a second implementation. The
//! SEC1 extractor below is deliberately independent of `lgwks_std::http`'s
//! own parser: a fixture built by the unit under test proves nothing. Ring's
//! `generate_pkcs8` omits parameters from its `ECPrivateKey`, while the
//! loader under test proves the curve, so the SEC1 below is rebuilt in the
//! traditional shape with explicit P-256 parameters before it is wrapped.

use lgwks_std::seeded::Seeded;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;

/// The estate's seeded stream as the RNG the RSA generator needs.
///
/// Deterministic per seed, so one seed names one PKI and a failure names the
/// seed that reproduces it. Infallible by construction: a seeded stream never
/// runs out.
struct SeededRng(Seeded);

impl rand_core::TryRng for SeededRng {
    /// Infallible by construction: a seeded stream never runs out.
    type Error = core::convert::Infallible;

    /// The low 32 bits of the next word, without a narrowing cast.
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let word = self.0.next_u64().to_le_bytes();
        Ok(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
    }

    /// The next word whole.
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0.next_u64())
    }

    /// Fill `dest` from the stream, eight bytes at a time.
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
        for piece in dest.chunks_mut(8) {
            let word = self.0.next_u64().to_le_bytes();
            piece.copy_from_slice(&word[..piece.len()]);
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for SeededRng {}

/// Wrap `der` in PEM with `label`, 64 columns like `openssl` writes.
pub fn pem_wrap(label: &str, der: &[u8]) -> Vec<u8> {
    let encoded = lgwks_std::encoding::base64::encode(der);
    let mut pem = format!("-----BEGIN {label}-----\n");
    let mut rest = encoded.as_str();
    while rest.len() > 64 {
        let (line, tail) = rest.split_at(64);
        pem.push_str(line);
        pem.push('\n');
        rest = tail;
    }
    pem.push_str(rest);
    pem.push('\n');
    pem.push_str("-----END ");
    pem.push_str(label);
    pem.push_str("-----\n");
    pem.into_bytes()
}

/// One DER type-length-value, read only far enough to skip it.
struct Tlv<'bytes> {
    /// The tag byte as read.
    tag: u8,
    /// The value bytes.
    body: &'bytes [u8],
    /// The full encoding, tag and length included, for verbatim reuse.
    span: &'bytes [u8],
}

impl<'bytes> Tlv<'bytes> {
    /// Read one TLV from the front of `input`.
    fn read(input: &'bytes [u8]) -> Option<(Self, &'bytes [u8])> {
        let (&tag, after_tag) = input.split_first()?;
        let (&first_len, after_len) = after_tag.split_first()?;
        let (body_len, header_len) = if first_len < 0x80 {
            (usize::from(first_len), 2_usize)
        } else {
            let count = usize::from(first_len ^ 0x80);
            if count == 0 || count > 4 || after_len.len() < count {
                return None;
            }
            let mut length = 0_usize;
            for byte in after_len.iter().take(count) {
                length = length
                    .saturating_mul(256)
                    .saturating_add(usize::from(*byte));
            }
            (length, 2_usize.saturating_add(count))
        };
        let total_len = header_len.checked_add(body_len)?;
        let span = input.get(..total_len)?;
        let body = span.get(header_len..total_len)?;
        let tail = input.get(total_len..)?;
        Some((Self { tag, body, span }, tail))
    }
}

/// Extract the SEC1 body from a PKCS#8 EC private key.
///
/// PKCS#8 wraps the SEC1 encoding whole as the privateKey OCTET STRING, so
/// this is a structural unwrap, not a re-encoding: the bytes below are the
/// same ones the PKCS#8 form carries. Independent of the loader under test
/// by design: a fixture built by the unit under test proves nothing.
fn sec1_of_pkcs8(der: &[u8]) -> Result<Vec<u8>, String> {
    let (outer, _) = Tlv::read(der).ok_or("test PKI: PKCS#8 is not DER")?;
    if outer.tag != 0x30 {
        Err("test PKI: PKCS#8 is not a sequence".to_owned())
    } else {
        sec1_key_of(outer.body)
    }
}

/// Skip a PKCS#8 body's version and algorithm, returning its key bytes.
fn sec1_key_of(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut rest = body;
    for _ in 0..2 {
        let (_, tail) = Tlv::read(rest).ok_or("test PKI: PKCS#8 is too short")?;
        rest = tail;
    }
    let (key, _) = Tlv::read(rest).ok_or("test PKI: PKCS#8 has no key")?;
    if key.tag != 0x04 || key.body.is_empty() {
        Err("test PKI: PKCS#8 key is not an octet string".to_owned())
    } else {
        Ok(key.body.to_vec())
    }
}

/// The explicit P-256 parameters field the traditional SEC1 shape carries:
/// `[0]` wrapping the P-256 OID (RFC 5480 section 2.1.1). Named here rather
/// than imported from the loader under test, which is what the point of an
/// independent fixture forbids.
const P256_PARAMS_FIELD: &[u8] = &[
    0xA0, 0x0A, 0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07,
];

/// Rebuild ring's parameter-less SEC1 as the traditional `openssl ec` shape.
///
/// Version, private-key and public-key spans are carried over verbatim from
/// the PKCS#8 OCTET STRING; one explicit parameters field naming P-256 is
/// inserted before the public key. Only P-256 fits the short-form lengths
/// below, which is exactly the curve the test PKI generates; anything else
/// is refused rather than mis-encoded.
fn sec1_with_params(sec1: &[u8]) -> Result<Vec<u8>, String> {
    let (outer, trailing) = Tlv::read(sec1).ok_or("test PKI: SEC1 is not DER")?;
    if outer.tag != 0x30 || !trailing.is_empty() {
        Err("test PKI: SEC1 is not one sequence".to_owned())
    } else {
        sec1_assembled(&child_spans(outer.body)?)
    }
}

/// Read every child span of `body`, so the caller can splice them verbatim.
fn child_spans(mut rest: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut spans = Vec::new();
    while !rest.is_empty() {
        let (child, tail) = Tlv::read(rest).ok_or("test PKI: SEC1 child is not DER")?;
        spans.push(child.span);
        rest = tail;
    }
    Ok(spans)
}

/// Splice SEC1 child spans with an explicit P-256 parameters field.
///
/// The spans are ring's version, private key and public key in order; the
/// parameters field is what turns the parameter-less encoding into the shape
/// the loader admits.
fn sec1_assembled(spans: &[&[u8]]) -> Result<Vec<u8>, String> {
    if spans.len() != 3 || spans[0] != [0x02, 0x01, 0x01] {
        Err("test PKI: SEC1 is not version, key and point".to_owned())
    } else {
        let (version, key, point) = (spans[0], spans[1], spans[2]);
        let mut body_len = P256_PARAMS_FIELD.len();
        for span in spans {
            body_len = body_len.saturating_add(span.len());
        }
        if body_len >= 0x80 {
            Err("test PKI: SEC1 does not fit short-form length".to_owned())
        } else {
            let len_byte = u8::try_from(body_len)
                .map_err(|error| format!("test PKI: SEC1 length refused: {error:?}"))?;
            let mut rebuilt = Vec::with_capacity(body_len.saturating_add(2));
            rebuilt.push(0x30);
            rebuilt.push(len_byte);
            rebuilt.extend_from_slice(version);
            rebuilt.extend_from_slice(key);
            rebuilt.extend_from_slice(P256_PARAMS_FIELD);
            rebuilt.extend_from_slice(point);
            Ok(rebuilt)
        }
    }
}

/// A throwaway PKI: a CA, an EC server with SANs, an RSA client, an unrelated
/// RSA key for the mismatch proof, and an Ed25519 pair for the refusal
/// proof. Every byte below is generated; nothing is read from disk.
#[derive(Debug, Clone)]
pub struct TestPki {
    /// The CA certificate: the only trust root the tests configure.
    ca_der: Vec<u8>,
    /// The server certificate, with `localhost` and `127.0.0.1` SANs.
    server_der: Vec<u8>,
    /// The server key in SEC1 (`EC PRIVATE KEY`) form, with explicit
    /// parameters: the traditional shape the loader admits.
    server_key_sec1_der: Vec<u8>,
    /// The server key in PKCS#8 (`PRIVATE KEY`) form.
    server_key_pkcs8_der: Vec<u8>,
    /// The client certificate, signed by the CA.
    client_der: Vec<u8>,
    /// The client key in PKCS#8 form.
    client_key_pkcs8_der: Vec<u8>,
    /// The client key in PKCS#1 (`RSA PRIVATE KEY`) form.
    client_key_pkcs1_der: Vec<u8>,
    /// An unrelated RSA key in PKCS#8 form, matching nothing.
    wrong_key_pkcs8_der: Vec<u8>,
    /// An Ed25519 certificate, refused as client identity by policy.
    ed_cert_der: Vec<u8>,
    /// An Ed25519 key in PKCS#8 form, refused as client identity by policy.
    ed_key_der: Vec<u8>,
}

/// Generate the throwaway PKI named by `seed`.
pub fn test_pki(seed: u64) -> Result<TestPki, String> {
    let mut rng = SeededRng(Seeded::from_seed(seed));
    let use_rng = &mut rng;

    // The CA: EC, self-signed, valid for the window the defaults carry.
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "lgwks test ca");
    let ca_key = rcgen::KeyPair::generate().map_err(|error| format!("test PKI: {error:?}"))?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|error| format!("test PKI: {error:?}"))?;
    let ca_issuer = rcgen::Issuer::from_params(&ca_params, ca_key);
    let ca_der = ca_cert.der().to_vec();

    // The server: EC, localhost and loopback SANs, signed by the CA.
    let server_key = rcgen::KeyPair::generate().map_err(|error| format!("test PKI: {error:?}"))?;
    let server_key_pkcs8_der = server_key.serialized_der().to_vec();
    let mut server_params =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .map_err(|error| format!("test PKI: {error:?}"))?;
    server_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let server_cert = server_params
        .signed_by(&server_key, &ca_issuer)
        .map_err(|error| format!("test PKI: {error:?}"))?;

    // The client: RSA-2048, signed by the CA.
    let client_rsa =
        rsa::RsaPrivateKey::new(use_rng, 2048).map_err(|error| format!("test PKI: {error:?}"))?;
    let client_pkcs8 = client_rsa
        .to_pkcs8_der()
        .map_err(|error| format!("test PKI: {error:?}"))?;
    let client_pkcs8_der = client_pkcs8.as_bytes().to_vec();
    let client_key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
        &client_pkcs8_der.as_slice().into(),
        &rcgen::PKCS_RSA_SHA256,
    )
    .map_err(|error| format!("test PKI: {error:?}"))?;
    let mut client_params = rcgen::CertificateParams::default();
    client_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "lgwks test client");
    let client_cert = client_params
        .signed_by(&client_key, &ca_issuer)
        .map_err(|error| format!("test PKI: {error:?}"))?;

    // An unrelated RSA key: the mismatch proof.
    let wrong_rsa =
        rsa::RsaPrivateKey::new(use_rng, 2048).map_err(|error| format!("test PKI: {error:?}"))?;
    let wrong_pkcs8 = wrong_rsa
        .to_pkcs8_der()
        .map_err(|error| format!("test PKI: {error:?}"))?;

    // An Ed25519 pair: refused as client identity by policy.
    let ed_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
        .map_err(|error| format!("test PKI: {error:?}"))?;
    let ed_params = rcgen::CertificateParams::default();
    let ed_cert = ed_params
        .self_signed(&ed_key)
        .map_err(|error| format!("test PKI: {error:?}"))?;

    Ok(TestPki {
        ca_der,
        server_der: server_cert.der().to_vec(),
        server_key_sec1_der: sec1_with_params(&sec1_of_pkcs8(&server_key_pkcs8_der)?)?,
        server_key_pkcs8_der,
        client_der: client_cert.der().to_vec(),
        client_key_pkcs8_der: client_pkcs8_der,
        client_key_pkcs1_der: client_rsa
            .to_pkcs1_der()
            .map_err(|error| format!("test PKI: {error:?}"))?
            .as_bytes()
            .to_vec(),
        wrong_key_pkcs8_der: wrong_pkcs8.as_bytes().to_vec(),
        ed_cert_der: ed_cert.der().to_vec(),
        ed_key_der: ed_key.serialized_der().to_vec(),
    })
}

/// The test PKI as PEM blocks: what the shipped client configures, and what
/// the loopback server parses back into rustls.
///
/// Both sides read the same blocks: the server proves they parse as DER,
/// the client proves they build through the shipped loader.
#[derive(Debug, Clone)]
pub struct TestPkiPems {
    /// The CA certificate: the only trust root the tests configure.
    pub(crate) ca: Vec<u8>,
    /// The server certificate, with `localhost` and `127.0.0.1` SANs.
    pub(crate) server: Vec<u8>,
    /// The server key in SEC1 (`EC PRIVATE KEY`) form, with explicit
    /// parameters: the traditional shape the loader admits.
    pub(crate) server_key_sec1: Vec<u8>,
    /// The server key in PKCS#8 (`PRIVATE KEY`) form.
    pub(crate) server_key_pkcs8: Vec<u8>,
    /// The client certificate, signed by the CA.
    pub(crate) client: Vec<u8>,
    /// The client key in PKCS#8 form.
    pub(crate) client_key: Vec<u8>,
    /// The client key in PKCS#1 (`RSA PRIVATE KEY`) form.
    pub(crate) client_key_pkcs1: Vec<u8>,
    /// An unrelated RSA key in PKCS#8 form, matching nothing.
    pub(crate) wrong_key: Vec<u8>,
    /// An Ed25519 certificate, refused as client identity by policy.
    pub(crate) ed_cert: Vec<u8>,
    /// An Ed25519 key in PKCS#8 form, refused as client identity by policy.
    pub(crate) ed_key: Vec<u8>,
}

/// Generate the throwaway PKI named by `seed`, as PEM blocks.
///
/// One generation serves both harnesses: the loopback server parses the same
/// blocks the shipped client configures, so a second generation (and a
/// second RSA keygen) would only burn time.
pub fn test_pki_pems(seed: u64) -> Result<TestPkiPems, String> {
    let pki = test_pki(seed)?;
    Ok(TestPkiPems {
        ca: pem_wrap("CERTIFICATE", &pki.ca_der),
        server: pem_wrap("CERTIFICATE", &pki.server_der),
        server_key_sec1: pem_wrap("EC PRIVATE KEY", &pki.server_key_sec1_der),
        server_key_pkcs8: pem_wrap("PRIVATE KEY", &pki.server_key_pkcs8_der),
        client: pem_wrap("CERTIFICATE", &pki.client_der),
        client_key: pem_wrap("PRIVATE KEY", &pki.client_key_pkcs8_der),
        client_key_pkcs1: pem_wrap("RSA PRIVATE KEY", &pki.client_key_pkcs1_der),
        wrong_key: pem_wrap("PRIVATE KEY", &pki.wrong_key_pkcs8_der),
        ed_cert: pem_wrap("CERTIFICATE", &pki.ed_cert_der),
        ed_key: pem_wrap("PRIVATE KEY", &pki.ed_key_der),
    })
}
