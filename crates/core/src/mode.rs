pub const OREAD: u8 = 0;
pub const OWRITE: u8 = 1;
pub const ORDWR: u8 = 2;
pub const OEXEC: u8 = 3;
pub const OTRUNC: u8 = 0x10;
pub const ORCLOSE: u8 = 0x40;

pub const ACCESS_MASK: u8 = 0x03;
pub const KNOWN_MODE_BITS: u8 = ACCESS_MASK | OTRUNC | ORCLOSE;

pub const fn is_valid(mode: u8) -> bool {
    mode & !KNOWN_MODE_BITS == 0
}

pub const fn permits_read(mode: u8) -> bool {
    matches!(mode & ACCESS_MASK, OREAD | ORDWR | OEXEC)
}

pub const fn permits_write(mode: u8) -> bool {
    matches!(mode & ACCESS_MASK, OWRITE | ORDWR)
}

pub const fn is_directory_mode(mode: u8) -> bool {
    is_valid(mode) && mode & !ORCLOSE == OREAD
}

#[cfg(test)]
mod tests {
    use super::{is_directory_mode, ORCLOSE, OREAD, OTRUNC, OWRITE};

    #[test]
    fn directory_mode_allows_remove_on_close_but_not_write_or_truncate() {
        assert!(is_directory_mode(OREAD));
        assert!(is_directory_mode(OREAD | ORCLOSE));
        assert!(!is_directory_mode(OWRITE));
        assert!(!is_directory_mode(OREAD | OTRUNC));
    }
}
