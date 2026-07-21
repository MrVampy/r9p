use crate::NOISE_PATTERN;
use r9p::error::{Error, Result};
use snow::params::NoiseParams;
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
const KEY_HEX_BYTES: usize = KEY_BYTES * 2;
#[derive(Clone)]
pub struct PrivateKey(Arc<PrivateKeyBytes>);

struct PrivateKeyBytes {
    bytes: [u8; KEY_BYTES],
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct PublicKey([u8; KEY_BYTES]);

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
        let metadata = fs::metadata(path).map_err(|error| {
            Error::from(format!("inspect private key {}: {error}", path.display()))
        })?;
        if !metadata.is_file() {
            return Err(Error::from(format!(
                "private key {} is not a regular file",
                path.display()
            )));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(Error::from(format!(
                "private key {} must not be accessible by group or other users",
                path.display()
            )));
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
    let params: NoiseParams = NOISE_PATTERN
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

pub fn write_key_pair(private_path: &Path, public_path: &Path, pair: &KeyPair) -> Result<()> {
    if private_path == public_path {
        return Err(Error::from("private and public key paths must differ"));
    }
    write_new_file(
        private_path,
        pair.private.render().as_bytes(),
        0o600,
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

fn write_new_file(path: &Path, bytes: &[u8], mode: u32, label: &str) -> Result<()> {
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

fn key_array(bytes: &[u8], label: &str) -> Result<[u8; KEY_BYTES]> {
    bytes
        .try_into()
        .map_err(|_| Error::from(format!("{label} must be {KEY_BYTES} bytes")))
}

fn decode_key(value: &str) -> Result<[u8; KEY_BYTES]> {
    if value.len() != KEY_HEX_BYTES {
        return Err(Error::from(format!(
            "key must be exactly {KEY_HEX_BYTES} lowercase hexadecimal characters"
        )));
    }
    let mut out = [0_u8; KEY_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        out[index] = (high << 4) | low;
    }
    Ok(out)
}

fn decode_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(Error::from("key must use lowercase hexadecimal")),
    }
}

fn encode_key(bytes: &[u8; KEY_BYTES]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(KEY_HEX_BYTES);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn generated_keys_round_trip_through_files() -> Result<()> {
        let serial = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::from(error.to_string()))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("r9p-auth-key-test-{serial}"));
        fs::create_dir(&root).map_err(|error| Error::from(error.to_string()))?;
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
        let root = std::env::temp_dir().join(format!("r9p-auth-mode-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).map_err(|error| Error::from(error.to_string()))?;
        let path = root.join("private");
        fs::write(&path, "00".repeat(KEY_BYTES)).map_err(|error| Error::from(error.to_string()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .map_err(|error| Error::from(error.to_string()))?;
        assert!(PrivateKey::read(&path).is_err());
        fs::remove_dir_all(root).map_err(|error| Error::from(error.to_string()))
    }

    #[test]
    fn key_generation_refuses_to_replace_existing_paths() -> Result<()> {
        let serial = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::from(error.to_string()))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("r9p-auth-no-replace-test-{serial}"));
        fs::create_dir(&root).map_err(|error| Error::from(error.to_string()))?;
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
}
