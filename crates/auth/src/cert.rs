//! Certificates that bind a name to a session key.
//!
//! `r9p-session-auth.v1` authenticates a key and then takes the name from a
//! `peer <key> <name>` line the *relying party* stores. The key is proven; the
//! name is asserted locally, so two servers may disagree about what one key is
//! called, and renaming a principal is a fleet-wide edit.
//!
//! A certificate moves the name into signed material. The relying party learns
//! it during the handshake instead of asserting it, which is what removes the
//! per-server peer list and makes a rename one re-signing.
//!
//! Two key types are in play and they are not interchangeable. Session keys are
//! X25519 Noise statics — Diffie-Hellman keys, which cannot sign. So the
//! issuing root is Ed25519 and signs *over* the subject's X25519 public key.
//! Nebula splits them the same way for the same reason.
//!
//! The signature covers a canonical, length-framed encoding built from the
//! parsed fields, never the file text. Reformatting a certificate cannot change
//! what was signed, and the framing means no field boundary can be shifted —
//! `name "a" group "b"` cannot be re-read as `name "a b"`.

use crate::{
    key::{decode_hex, encode_hex, path_exists, write_new_file},
    PublicKey,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use r9p::error::{Error, Result};
use std::{
    collections::BTreeSet,
    fmt, fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub const CERT_FORMAT: &str = "r9p-cert.v1";

const KEY_BYTES: usize = 32;
const SIGNATURE_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 255;
const MAX_GROUPS: usize = 32;

/// Seconds since the Unix epoch. Stored as an integer rather than a formatted
/// timestamp so there is no calendar library in the trust path and no parser
/// disagreement about what a certificate says.
pub type UnixSeconds = u64;

pub fn now_unix() -> Result<UnixSeconds> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| Error::from("system clock is before the Unix epoch"))
}

/// The Ed25519 signing root. Offline by design: it lives in sops and is used by
/// an operator command, never by a running service.
pub struct RootPrivateKey(SigningKey);

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RootPublicKey(VerifyingKey);

pub struct RootKeyPair {
    pub private: RootPrivateKey,
    pub public: RootPublicKey,
}

impl RootPrivateKey {
    pub fn from_hex(value: &str) -> Result<Self> {
        let bytes = decode_hex::<KEY_BYTES>(value, "root private key")?;
        Ok(Self(SigningKey::from_bytes(&bytes)))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let value = read_private(path)?;
        Self::from_hex(value.trim())
    }

    pub fn public(&self) -> RootPublicKey {
        RootPublicKey(self.0.verifying_key())
    }

    fn render(&self) -> String {
        encode_hex(&self.0.to_bytes())
    }
}

impl fmt::Debug for RootPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RootPrivateKey([redacted])")
    }
}

impl RootPublicKey {
    pub fn from_hex(value: &str) -> Result<Self> {
        let bytes = decode_hex::<KEY_BYTES>(value, "root public key")?;
        VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|_| Error::from("root public key is not a valid Ed25519 point"))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let value = fs::read_to_string(path).map_err(|error| {
            Error::from(format!("read root public key {}: {error}", path.display()))
        })?;
        Self::from_hex(value.trim())
    }

    pub fn to_hex(self) -> String {
        encode_hex(&self.0.to_bytes())
    }
}

impl fmt::Debug for RootPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RootPublicKey")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for RootPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

pub fn generate_root_key_pair() -> Result<RootKeyPair> {
    let mut seed = [0_u8; KEY_BYTES];
    getrandom::fill(&mut seed)
        .map_err(|error| Error::from(format!("draw root key material: {error}")))?;
    let private = RootPrivateKey(SigningKey::from_bytes(&seed));
    let public = private.public();
    Ok(RootKeyPair { private, public })
}

/// Converges on an existing root the way `provision_key_pair` does for session
/// keys: derive a missing public half, verify a present one, and refuse to
/// replace a public key whose private half is gone.
pub fn provision_root_key_pair(private_path: &Path, public_path: &Path) -> Result<RootKeyPair> {
    if private_path == public_path {
        return Err(Error::from("private and public key paths must differ"));
    }
    match (
        path_exists(private_path, "root private key")?,
        path_exists(public_path, "root public key")?,
    ) {
        (false, false) => {
            let pair = generate_root_key_pair()?;
            write_new_file(
                private_path,
                pair.private.render().as_bytes(),
                0o600,
                "root private key",
            )?;
            match write_new_file(
                public_path,
                pair.public.to_hex().as_bytes(),
                0o644,
                "root public key",
            ) {
                Ok(()) => Ok(pair),
                Err(error) => {
                    let _ = fs::remove_file(private_path);
                    Err(error)
                }
            }
        }
        (true, false) => {
            let private = RootPrivateKey::read(private_path)?;
            let public = private.public();
            write_new_file(
                public_path,
                public.to_hex().as_bytes(),
                0o644,
                "root public key",
            )?;
            Ok(RootKeyPair { private, public })
        }
        (true, true) => {
            let private = RootPrivateKey::read(private_path)?;
            let public = RootPublicKey::read(public_path)?;
            if private.public() != public {
                return Err(Error::from(format!(
                    "root public key {} does not match private key {}",
                    public_path.display(),
                    private_path.display()
                )));
            }
            Ok(RootKeyPair { private, public })
        }
        (false, true) => Err(Error::from(format!(
            "root public key {} exists without private key {}; refusing to replace it",
            public_path.display(),
            private_path.display()
        ))),
    }
}

/// Everything a signature commits to. Kept separate from the signature so a
/// signature can only ever be checked against the fields that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateBody {
    name: String,
    key: PublicKey,
    groups: BTreeSet<String>,
    not_before: UnixSeconds,
    not_after: UnixSeconds,
    issuer: RootPublicKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Certificate {
    body: CertificateBody,
    signature: [u8; SIGNATURE_BYTES],
}

impl CertificateBody {
    pub fn new(
        name: impl Into<String>,
        key: PublicKey,
        groups: impl IntoIterator<Item = String>,
        not_before: UnixSeconds,
        not_after: UnixSeconds,
        issuer: RootPublicKey,
    ) -> Result<Self> {
        let name = name.into();
        validate_label(&name, "certificate name")?;
        let mut collected = BTreeSet::new();
        for group in groups {
            validate_label(&group, "certificate group")?;
            collected.insert(group);
        }
        if collected.len() > MAX_GROUPS {
            return Err(Error::from(format!(
                "certificate carries more than {MAX_GROUPS} groups"
            )));
        }
        if not_before >= not_after {
            return Err(Error::from("certificate not-before must precede not-after"));
        }
        Ok(Self {
            name,
            key,
            groups: collected,
            not_before,
            not_after,
            issuer,
        })
    }

    /// Canonical signed bytes. Every variable-length field is preceded by its
    /// big-endian length, and the format tag is included so a future version
    /// cannot be reinterpreted as this one.
    fn signing_input(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        push_framed(&mut out, CERT_FORMAT.as_bytes())?;
        push_framed(&mut out, self.name.as_bytes())?;
        push_framed(&mut out, self.key.as_bytes())?;
        let count = u32::try_from(self.groups.len())
            .map_err(|_| Error::from("certificate group count does not fit in u32"))?;
        out.extend_from_slice(&count.to_be_bytes());
        for group in &self.groups {
            push_framed(&mut out, group.as_bytes())?;
        }
        out.extend_from_slice(&self.not_before.to_be_bytes());
        out.extend_from_slice(&self.not_after.to_be_bytes());
        push_framed(&mut out, &self.issuer.0.to_bytes())?;
        Ok(out)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn key(&self) -> PublicKey {
        self.key
    }

    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.groups.iter().map(String::as_str)
    }

    pub const fn not_before(&self) -> UnixSeconds {
        self.not_before
    }

    pub const fn not_after(&self) -> UnixSeconds {
        self.not_after
    }

    pub const fn issuer(&self) -> RootPublicKey {
        self.issuer
    }
}

impl Certificate {
    pub fn sign(root: &RootPrivateKey, body: CertificateBody) -> Result<Self> {
        if body.issuer != root.public() {
            return Err(Error::from(
                "certificate issuer does not match the signing root",
            ));
        }
        let signature = root.0.sign(&body.signing_input()?);
        Ok(Self {
            body,
            signature: signature.to_bytes(),
        })
    }

    /// Checks issuer, then signature, then validity window. Signature first, so
    /// a forged certificate is reported as forged rather than as expired.
    pub fn verify(&self, root: RootPublicKey, now: UnixSeconds) -> Result<()> {
        if self.body.issuer != root {
            return Err(Error::from(format!(
                "certificate for {} was issued by root {}, not {}",
                self.body.name, self.body.issuer, root
            )));
        }
        let signature = Signature::from_bytes(&self.signature);
        root.0
            .verify_strict(&self.body.signing_input()?, &signature)
            .map_err(|_| {
                Error::from(format!(
                    "certificate signature for {} is not valid",
                    self.body.name
                ))
            })?;
        if now < self.body.not_before {
            return Err(Error::from(format!(
                "certificate for {} is not valid until {}",
                self.body.name, self.body.not_before
            )));
        }
        if now >= self.body.not_after {
            return Err(Error::from(format!(
                "certificate for {} expired at {}",
                self.body.name, self.body.not_after
            )));
        }
        Ok(())
    }

    pub const fn body(&self) -> &CertificateBody {
        &self.body
    }

    pub const fn signature(&self) -> &[u8; SIGNATURE_BYTES] {
        &self.signature
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("format {CERT_FORMAT}\n"));
        out.push_str(&format!("name {}\n", self.body.name));
        out.push_str(&format!("key {}\n", self.body.key.to_hex()));
        for group in &self.body.groups {
            out.push_str(&format!("group {group}\n"));
        }
        out.push_str(&format!("not-before {}\n", self.body.not_before));
        out.push_str(&format!("not-after {}\n", self.body.not_after));
        out.push_str(&format!("issuer {}\n", self.body.issuer));
        out.push_str(&format!("signature {}\n", encode_hex(&self.signature)));
        out
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut format = None;
        let mut name = None;
        let mut key = None;
        let mut groups = Vec::new();
        let mut not_before = None;
        let mut not_after = None;
        let mut issuer = None;
        let mut signature = None;

        for (index, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let field = fields.next().unwrap_or_default();
            let values = fields.collect::<Vec<_>>();
            match (field, values.as_slice()) {
                ("format", [value]) => set_once(&mut format, value, "format")?,
                ("name", [value]) => set_once(&mut name, value, "name")?,
                ("key", [value]) => set_once(&mut key, value, "key")?,
                ("group", [value]) => groups.push((*value).to_string()),
                ("not-before", [value]) => set_once(&mut not_before, value, "not-before")?,
                ("not-after", [value]) => set_once(&mut not_after, value, "not-after")?,
                ("issuer", [value]) => set_once(&mut issuer, value, "issuer")?,
                ("signature", [value]) => set_once(&mut signature, value, "signature")?,
                _ => {
                    return Err(Error::from(format!(
                        "invalid certificate line {}",
                        index + 1
                    )));
                }
            }
        }

        if format.as_deref() != Some(CERT_FORMAT) {
            return Err(Error::from(format!(
                "certificate format must be {CERT_FORMAT}"
            )));
        }
        let body = CertificateBody::new(
            required(name, "name")?,
            PublicKey::from_hex(&required(key, "key")?)?,
            groups,
            parse_seconds(&required(not_before, "not-before")?, "not-before")?,
            parse_seconds(&required(not_after, "not-after")?, "not-after")?,
            RootPublicKey::from_hex(&required(issuer, "issuer")?)?,
        )?;
        Ok(Self {
            body,
            signature: decode_hex::<SIGNATURE_BYTES>(
                &required(signature, "signature")?,
                "certificate signature",
            )?,
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let input = fs::read_to_string(path).map_err(|error| {
            Error::from(format!("read certificate {}: {error}", path.display()))
        })?;
        Self::parse(&input)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        // 0644: a certificate is public material. Only the root private key and
        // the subject's own session key are secret.
        write_new_file(
            path,
            self.render().trim_end().as_bytes(),
            0o644,
            "certificate",
        )
    }
}

fn push_framed(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| Error::from("certificate field does not fit in u32"))?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Names and groups appear on whitespace-delimited lines and are compared as
/// exact strings, so whitespace is rejected rather than silently mangled on the
/// round trip. This is stricter than `validate_principal`, which permits it.
fn validate_label(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_LABEL_BYTES {
        return Err(Error::from(format!(
            "{what} must contain 1 to {MAX_LABEL_BYTES} bytes"
        )));
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(Error::from(format!("{what} contains a control byte")));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(Error::from(format!("{what} contains whitespace")));
    }
    if value.starts_with('#') {
        return Err(Error::from(format!("{what} starts with a comment marker")));
    }
    Ok(())
}

fn parse_seconds(value: &str, field: &str) -> Result<UnixSeconds> {
    value.parse::<UnixSeconds>().map_err(|_| {
        Error::from(format!(
            "certificate {field} must be whole seconds since the Unix epoch"
        ))
    })
}

fn set_once(target: &mut Option<String>, value: &str, field: &str) -> Result<()> {
    if target.replace(value.to_string()).is_some() {
        Err(Error::from(format!("duplicate certificate field {field}")))
    } else {
        Ok(())
    }
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    value.ok_or_else(|| Error::from(format!("missing certificate field {field}")))
}

fn read_private(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).map_err(|error| {
        Error::from(format!(
            "inspect root private key {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(Error::from(format!(
            "root private key {} is not a regular file",
            path.display()
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(Error::from(format!(
            "root private key {} must not be accessible by group or other users",
            path.display()
        )));
    }
    fs::read_to_string(path)
        .map_err(|error| Error::from(format!("read root private key {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_key_pair;

    const T0: UnixSeconds = 1_785_000_000;
    const T1: UnixSeconds = 1_848_000_000;

    fn signed(name: &str, groups: &[&str]) -> Result<(RootKeyPair, PublicKey, Certificate)> {
        let root = generate_root_key_pair()?;
        let subject = generate_key_pair()?;
        let body = CertificateBody::new(
            name,
            subject.public,
            groups.iter().map(|group| (*group).to_string()),
            T0,
            T1,
            root.public,
        )?;
        let cert = Certificate::sign(&root.private, body)?;
        Ok((root, subject.public, cert))
    }

    #[test]
    fn a_signed_certificate_verifies_inside_its_window() -> Result<()> {
        let (root, subject, cert) = signed("tuxedo", &["operator", "laptop"])?;
        cert.verify(root.public, T0 + 1)?;
        assert_eq!(cert.body().name(), "tuxedo");
        assert_eq!(cert.body().key(), subject);
        assert_eq!(
            cert.body().groups().collect::<Vec<_>>(),
            vec!["laptop", "operator"]
        );
        Ok(())
    }

    #[test]
    fn verification_is_bounded_by_the_validity_window() -> Result<()> {
        let (root, _, cert) = signed("tuxedo", &[])?;
        assert!(cert.verify(root.public, T0 - 1).is_err());
        cert.verify(root.public, T0)?;
        cert.verify(root.public, T1 - 1)?;
        assert!(cert.verify(root.public, T1).is_err());
        Ok(())
    }

    #[test]
    fn another_root_cannot_vouch_for_the_certificate() -> Result<()> {
        let (_, _, cert) = signed("tuxedo", &[])?;
        let other = generate_root_key_pair()?;
        assert!(cert.verify(other.public, T0 + 1).is_err());
        Ok(())
    }

    #[test]
    fn text_round_trip_preserves_every_signed_field() -> Result<()> {
        let (root, _, cert) = signed("m7", &["server", "operator"])?;
        let parsed = Certificate::parse(&cert.render())?;
        assert_eq!(parsed, cert);
        parsed.verify(root.public, T0 + 1)?;
        Ok(())
    }

    #[test]
    fn comments_and_blank_lines_do_not_change_what_was_signed() -> Result<()> {
        let (root, _, cert) = signed("m7", &["server"])?;
        let decorated = format!("# issued by hand\n\n{}\n", cert.render());
        Certificate::parse(&decorated)?.verify(root.public, T0 + 1)?;
        Ok(())
    }

    #[test]
    fn editing_the_name_invalidates_the_signature() -> Result<()> {
        let (root, _, cert) = signed("tuxedo", &["operator"])?;
        let tampered = cert.render().replace("name tuxedo", "name m7");
        let parsed = Certificate::parse(&tampered)?;
        assert_eq!(parsed.body().name(), "m7");
        assert!(parsed.verify(root.public, T0 + 1).is_err());
        Ok(())
    }

    #[test]
    fn adding_a_group_invalidates_the_signature() -> Result<()> {
        let (root, _, cert) = signed("tuxedo", &["laptop"])?;
        let tampered = cert
            .render()
            .replace("group laptop", "group laptop\ngroup operator");
        let parsed = Certificate::parse(&tampered)?;
        assert!(parsed.verify(root.public, T0 + 1).is_err());
        Ok(())
    }

    #[test]
    fn extending_the_expiry_invalidates_the_signature() -> Result<()> {
        let (root, _, cert) = signed("tuxedo", &[])?;
        let tampered = cert
            .render()
            .replace(&format!("not-after {T1}"), &format!("not-after {}", T1 + 1));
        let parsed = Certificate::parse(&tampered)?;
        assert!(parsed.verify(root.public, T0 + 1).is_err());
        Ok(())
    }

    #[test]
    fn swapping_the_subject_key_invalidates_the_signature() -> Result<()> {
        let (root, subject, cert) = signed("tuxedo", &[])?;
        let other = generate_key_pair()?;
        let tampered = cert
            .render()
            .replace(&subject.to_hex(), &other.public.to_hex());
        let parsed = Certificate::parse(&tampered)?;
        assert!(parsed.verify(root.public, T0 + 1).is_err());
        Ok(())
    }

    #[test]
    fn field_boundaries_cannot_be_shifted_between_name_and_group() -> Result<()> {
        // Without length framing, name="a" group="b" and name="ab" group=""
        // could produce the same signed bytes.
        let root = generate_root_key_pair()?;
        let subject = generate_key_pair()?;
        let split =
            CertificateBody::new("a", subject.public, ["b".to_string()], T0, T1, root.public)?;
        let joined = CertificateBody::new(
            "ab",
            subject.public,
            Vec::<String>::new(),
            T0,
            T1,
            root.public,
        )?;
        assert_ne!(split.signing_input()?, joined.signing_input()?);
        Ok(())
    }

    #[test]
    fn signing_rejects_a_body_issued_by_a_different_root() -> Result<()> {
        let root = generate_root_key_pair()?;
        let other = generate_root_key_pair()?;
        let subject = generate_key_pair()?;
        let body = CertificateBody::new(
            "tuxedo",
            subject.public,
            Vec::<String>::new(),
            T0,
            T1,
            other.public,
        )?;
        assert!(Certificate::sign(&root.private, body).is_err());
        Ok(())
    }

    #[test]
    fn names_and_groups_reject_whitespace_and_control_bytes() -> Result<()> {
        let root = generate_root_key_pair()?;
        let subject = generate_key_pair()?;
        for bad in ["two words", "tab\there", "new\nline", "", "#comment"] {
            assert!(
                CertificateBody::new(
                    bad,
                    subject.public,
                    Vec::<String>::new(),
                    T0,
                    T1,
                    root.public
                )
                .is_err(),
                "accepted name {bad:?}"
            );
        }
        assert!(CertificateBody::new(
            "ok",
            subject.public,
            ["two words".to_string()],
            T0,
            T1,
            root.public
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn an_inverted_validity_window_is_rejected() -> Result<()> {
        let root = generate_root_key_pair()?;
        let subject = generate_key_pair()?;
        assert!(CertificateBody::new(
            "tuxedo",
            subject.public,
            Vec::<String>::new(),
            T1,
            T0,
            root.public
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn duplicate_fields_are_rejected_rather_than_last_wins() -> Result<()> {
        let (_, _, cert) = signed("tuxedo", &[])?;
        let duplicated = format!("{}name m7\n", cert.render());
        assert!(Certificate::parse(&duplicated).is_err());
        Ok(())
    }

    #[test]
    fn repeated_groups_collapse_and_order_does_not_matter() -> Result<()> {
        let root = generate_root_key_pair()?;
        let subject = generate_key_pair()?;
        let one = CertificateBody::new(
            "m7",
            subject.public,
            ["b".to_string(), "a".to_string(), "b".to_string()],
            T0,
            T1,
            root.public,
        )?;
        let two = CertificateBody::new(
            "m7",
            subject.public,
            ["a".to_string(), "b".to_string()],
            T0,
            T1,
            root.public,
        )?;
        assert_eq!(one.signing_input()?, two.signing_input()?);
        Ok(())
    }

    #[test]
    fn a_root_key_pair_round_trips_through_hex() -> Result<()> {
        let pair = generate_root_key_pair()?;
        let restored = RootPrivateKey::from_hex(&pair.private.render())?;
        assert_eq!(restored.public(), pair.public);
        assert_eq!(RootPublicKey::from_hex(&pair.public.to_hex())?, pair.public);
        Ok(())
    }
}
