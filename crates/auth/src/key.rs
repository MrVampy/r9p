use crate::NOISE_PATTERN_XX;
use r9p::error::{Error, Result};
use snow::{
    params::{DHChoice, NoiseParams},
    resolvers::{CryptoResolver, DefaultResolver},
};
use std::{
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::Arc,
};
use zeroize::{Zeroize, Zeroizing};

const KEY_BYTES: usize = 32;
#[derive(Clone)]
pub struct PrivateKey(Arc<PrivateKeyBytes>);

struct PrivateKeyBytes {
    bytes: [u8; KEY_BYTES],
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct PublicKey([u8; KEY_BYTES]);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PrivateKeyAccess {
    #[default]
    OwnerOnly,
    OwnerGroupRead,
}

#[derive(Clone)]
pub struct KeyPair {
    pub private: PrivateKey,
    pub public: PublicKey,
}

impl PrivateKey {
    pub fn from_hex(value: &str) -> Result<Self> {
        decode_key(value).map(|bytes| Self(Arc::new(PrivateKeyBytes { bytes })))
    }

    pub fn read(path: &Path) -> Result<Self> {
        Self::read_with_access(path, PrivateKeyAccess::OwnerOnly)
    }

    pub fn read_with_access(path: &Path, access: PrivateKeyAccess) -> Result<Self> {
        let metadata = fs::metadata(path).map_err(|error| {
            Error::from(format!("inspect private key {}: {error}", path.display()))
        })?;
        if !metadata.is_file() {
            return Err(Error::from(format!(
                "private key {} is not a regular file",
                path.display()
            )));
        }
        let mode = metadata.mode() & 0o777;
        match access {
            PrivateKeyAccess::OwnerOnly if mode & 0o077 != 0 => {
                return Err(Error::from(format!(
                    "private key {} must not be accessible by group or other users",
                    path.display()
                )));
            }
            PrivateKeyAccess::OwnerGroupRead if mode & 0o040 == 0 || mode & 0o037 != 0 => {
                return Err(Error::from(format!(
                    "private key {} must grant group read without group write, group execute, or other access",
                    path.display()
                )));
            }
            _ => {}
        }
        let value = fs::read_to_string(path).map_err(|error| {
            Error::from(format!("read private key {}: {error}", path.display()))
        })?;
        Self::from_hex(value.trim())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0.bytes
    }

    fn render(&self) -> String {
        encode_key(self.as_bytes())
    }
}

impl PrivateKeyAccess {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "owner-only" => Ok(Self::OwnerOnly),
            "owner-group-read" => Ok(Self::OwnerGroupRead),
            _ => Err(Error::from(format!(
                "private key access must be owner-only or owner-group-read, got {value}"
            ))),
        }
    }

    pub const fn file_mode(self) -> u32 {
        match self {
            Self::OwnerOnly => 0o600,
            Self::OwnerGroupRead => 0o640,
        }
    }
}

impl Drop for PrivateKeyBytes {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for PrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrivateKey([redacted])")
    }
}

impl PublicKey {
    pub fn from_hex(value: &str) -> Result<Self> {
        decode_key(value).map(Self)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let value = fs::read_to_string(path)
            .map_err(|error| Error::from(format!("read public key {}: {error}", path.display())))?;
        Self::from_hex(value.trim())
    }

    pub fn to_hex(self) -> String {
        encode_key(&self.0)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        key_array(bytes, "Noise public key").map(Self)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PublicKey")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

pub fn generate_key_pair() -> Result<KeyPair> {
    let params: NoiseParams = NOISE_PATTERN_XX
        .parse()
        .map_err(|error| Error::from(format!("parse Noise pattern: {error}")))?;
    let pair = snow::Builder::new(params)
        .generate_keypair()
        .map_err(|error| Error::from(format!("generate Noise key pair: {error}")))?;
    let snow::Keypair { private, public } = pair;
    let private = Zeroizing::new(private);
    let private = key_array(private.as_slice(), "generated private key")?;
    let public = key_array(&public, "generated public key")?;
    Ok(KeyPair {
        private: PrivateKey(Arc::new(PrivateKeyBytes { bytes: private })),
        public: PublicKey(public),
    })
}

pub fn provision_key_pair(private_path: &Path, public_path: &Path) -> Result<KeyPair> {
    provision_key_pair_with_access(private_path, public_path, PrivateKeyAccess::OwnerOnly)
}

pub fn provision_key_pair_with_access(
    private_path: &Path,
    public_path: &Path,
    access: PrivateKeyAccess,
) -> Result<KeyPair> {
    if private_path == public_path {
        return Err(Error::from("private and public key paths must differ"));
    }
    let private_exists = path_exists(private_path, "private key")?;
    let public_exists = path_exists(public_path, "public key")?;
    match (private_exists, public_exists) {
        (false, false) => {
            let pair = generate_key_pair()?;
            write_key_pair_with_access(private_path, public_path, &pair, access)?;
            Ok(pair)
        }
        (true, false) => {
            let private = PrivateKey::read_with_access(private_path, access)?;
            let public = derive_public_key(&private)?;
            write_new_file(public_path, public.to_hex().as_bytes(), 0o644, "public key")?;
            Ok(KeyPair { private, public })
        }
        (true, true) => {
            let private = PrivateKey::read_with_access(private_path, access)?;
            let public = PublicKey::read(public_path)?;
            if derive_public_key(&private)? != public {
                return Err(Error::from(format!(
                    "public key {} does not match private key {}",
                    public_path.display(),
                    private_path.display()
                )));
            }
            Ok(KeyPair { private, public })
        }
        (false, true) => Err(Error::from(format!(
            "public key {} exists without private key {}; refusing to replace it",
            public_path.display(),
            private_path.display()
        ))),
    }
}

pub fn write_key_pair(private_path: &Path, public_path: &Path, pair: &KeyPair) -> Result<()> {
    write_key_pair_with_access(private_path, public_path, pair, PrivateKeyAccess::OwnerOnly)
}

fn write_key_pair_with_access(
    private_path: &Path,
    public_path: &Path,
    pair: &KeyPair,
    access: PrivateKeyAccess,
) -> Result<()> {
    if private_path == public_path {
        return Err(Error::from("private and public key paths must differ"));
    }
    write_new_file(
        private_path,
        pair.private.render().as_bytes(),
        access.file_mode(),
        "private key",
    )?;
    match write_new_file(
        public_path,
        pair.public.to_hex().as_bytes(),
        0o644,
        "public key",
    ) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(private_path);
            Err(error)
        }
    }
}

pub(crate) fn write_new_file(path: &Path, bytes: &[u8], mode: u32, label: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| Error::from(format!("create {label} {}: {error}", path.display())))?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::set_permissions(path, fs::Permissions::from_mode(mode)));
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(path);
            Err(Error::from(format!(
                "write {label} {}: {error}",
                path.display()
            )))
        }
    }
}

pub(crate) fn derive_public_key(private: &PrivateKey) -> Result<PublicKey> {
    let resolver = DefaultResolver;
    let mut dh = resolver
        .resolve_dh(&DHChoice::Curve25519)
        .ok_or_else(|| Error::from("resolve Noise 25519 implementation"))?;
    dh.set(private.as_bytes());
    PublicKey::from_bytes(dh.pubkey())
}

pub(crate) fn path_exists(path: &Path, label: &str) -> Result<bool> {
    path.try_exists()
        .map_err(|error| Error::from(format!("inspect {label} {}: {error}", path.display())))
}

fn key_array(bytes: &[u8], label: &str) -> Result<[u8; KEY_BYTES]> {
    bytes
        .try_into()
        .map_err(|_| Error::from(format!("{label} must be {KEY_BYTES} bytes")))
}

fn decode_key(value: &str) -> Result<[u8; KEY_BYTES]> {
    decode_hex(value, "key")
}

/// Fixed-width lowercase hex, shared by 32-byte keys and 64-byte certificate
/// signatures so there is one decoder to get wrong rather than two.
pub(crate) fn decode_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N]> {
    let expected = N * 2;
    if value.len() != expected {
        return Err(Error::from(format!(
            "{label} must be exactly {expected} lowercase hexadecimal characters"
        )));
    }
    let mut out = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0], label)?;
        let low = decode_nibble(pair[1], label)?;
        out[index] = (high << 4) | low;
    }
    Ok(out)
}

fn decode_nibble(value: u8, label: &str) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(Error::from(format!(
            "{label} must use lowercase hexadecimal"
        ))),
    }
}

fn encode_key(bytes: &[u8; KEY_BYTES]) -> String {
    encode_hex(bytes)
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ROOT_SERIAL: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn generated_keys_round_trip_through_files() -> Result<()> {
        let root = test_root("round-trip")?;
        let private_path = root.join("private");
        let public_path = root.join("public");
        let pair = generate_key_pair()?;
        write_key_pair(&private_path, &public_path, &pair)?;

        assert_eq!(
            PrivateKey::read(&private_path)?.as_bytes(),
            pair.private.as_bytes()
        );
        assert_eq!(PublicKey::read(&public_path)?, pair.public);
        assert_eq!(
            fs::metadata(&private_path)
                .map_err(|error| Error::from(error.to_string()))?
                .mode()
                & 0o777,
            0o600
        );

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn private_key_rejects_group_readable_file() -> Result<()> {
        let root = test_root("mode")?;
        let path = root.join("private");
        fs::write(&path, "00".repeat(KEY_BYTES)).map_err(|error| Error::from(error.to_string()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .map_err(|error| Error::from(error.to_string()))?;
        assert!(PrivateKey::read(&path).is_err());
        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn private_key_accepts_only_explicit_group_read_access() -> Result<()> {
        let root = test_root("group-read")?;
        let path = root.join("private");
        fs::write(&path, "00".repeat(KEY_BYTES)).map_err(|error| Error::from(error.to_string()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .map_err(|error| Error::from(error.to_string()))?;

        PrivateKey::read_with_access(&path, PrivateKeyAccess::OwnerGroupRead)?;
        assert!(PrivateKey::read(&path).is_err());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o660))
            .map_err(|error| Error::from(error.to_string()))?;
        assert!(PrivateKey::read_with_access(&path, PrivateKeyAccess::OwnerGroupRead).is_err());

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn key_generation_refuses_to_replace_existing_paths() -> Result<()> {
        let root = test_root("no-replace")?;
        let private_path = root.join("private");
        let public_path = root.join("public");
        fs::write(&public_path, b"existing\n").map_err(|error| Error::from(error.to_string()))?;

        let pair = generate_key_pair()?;
        assert!(write_key_pair(&private_path, &public_path, &pair).is_err());
        assert!(!private_path.exists());
        assert_eq!(
            fs::read(&public_path).map_err(|error| Error::from(error.to_string()))?,
            b"existing\n"
        );

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn provisioning_creates_a_missing_pair() -> Result<()> {
        let root = test_root("provision-new")?;
        let private_path = root.join("private");
        let public_path = root.join("public");

        let provisioned = provision_key_pair(&private_path, &public_path)?;
        assert_eq!(
            PrivateKey::read(&private_path)?.as_bytes(),
            provisioned.private.as_bytes()
        );
        assert_eq!(PublicKey::read(&public_path)?, provisioned.public);

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn group_shared_provisioning_creates_and_reopens_a_group_readable_key() -> Result<()> {
        let root = test_root("provision-group-read")?;
        let private_path = root.join("private");
        let public_path = root.join("public");

        let provisioned = provision_key_pair_with_access(
            &private_path,
            &public_path,
            PrivateKeyAccess::OwnerGroupRead,
        )?;
        let reopened = provision_key_pair_with_access(
            &private_path,
            &public_path,
            PrivateKeyAccess::OwnerGroupRead,
        )?;
        assert_eq!(provisioned.private.as_bytes(), reopened.private.as_bytes());
        assert_eq!(
            fs::metadata(&private_path)
                .map_err(|error| Error::from(error.to_string()))?
                .mode()
                & 0o777,
            0o640
        );

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn provisioning_recovers_a_missing_public_key() -> Result<()> {
        let root = test_root("provision-recovery")?;
        let private_path = root.join("private");
        let public_path = root.join("public");
        let pair = generate_key_pair()?;
        write_key_pair(&private_path, &public_path, &pair)?;
        fs::remove_file(&public_path).map_err(|error| Error::from(error.to_string()))?;

        let provisioned = provision_key_pair(&private_path, &public_path)?;
        assert_eq!(provisioned.private.as_bytes(), pair.private.as_bytes());
        assert_eq!(provisioned.public, pair.public);
        assert_eq!(PublicKey::read(&public_path)?, pair.public);
        assert_eq!(
            fs::metadata(&public_path)
                .map_err(|error| Error::from(error.to_string()))?
                .mode()
                & 0o777,
            0o644
        );

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn provisioning_verifies_an_existing_pair() -> Result<()> {
        let root = test_root("provision-existing")?;
        let private_path = root.join("private");
        let public_path = root.join("public");
        let pair = generate_key_pair()?;
        write_key_pair(&private_path, &public_path, &pair)?;

        let provisioned = provision_key_pair(&private_path, &public_path)?;
        assert_eq!(provisioned.private.as_bytes(), pair.private.as_bytes());
        assert_eq!(provisioned.public, pair.public);

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn provisioning_rejects_a_public_key_without_its_private_key() -> Result<()> {
        let root = test_root("provision-public-only")?;
        let private_path = root.join("private");
        let public_path = root.join("public");
        let pair = generate_key_pair()?;
        fs::write(&public_path, format!("{}\n", pair.public))
            .map_err(|error| Error::from(error.to_string()))?;

        assert!(provision_key_pair(&private_path, &public_path).is_err());
        assert!(!private_path.exists());
        assert_eq!(PublicKey::read(&public_path)?, pair.public);

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn provisioning_rejects_a_mismatched_pair() -> Result<()> {
        let root = test_root("provision-mismatch")?;
        let private_path = root.join("private");
        let public_path = root.join("public");
        let pair = generate_key_pair()?;
        let other = generate_key_pair()?;
        write_key_pair(&private_path, &public_path, &pair)?;
        fs::write(&public_path, format!("{}\n", other.public))
            .map_err(|error| Error::from(error.to_string()))?;

        assert!(provision_key_pair(&private_path, &public_path).is_err());
        assert_eq!(PublicKey::read(&public_path)?, other.public);

        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    fn test_root(label: &str) -> Result<std::path::PathBuf> {
        let serial = TEST_ROOT_SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "r9p-auth-{label}-test-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&root).map_err(|error| Error::from(error.to_string()))?;
        Ok(root)
    }
}
