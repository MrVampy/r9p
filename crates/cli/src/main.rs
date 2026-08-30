mod args;
mod commands;
mod errors;
mod format;
mod io;
mod target;
mod transport;

pub(crate) const DEFAULT_MSIZE: u32 = r9p::codec::MAX_MSIZE;
pub(crate) const READ_CHUNK: u32 = r9p::codec::MAX_MSIZE;
pub(crate) const CTRL_R: u8 = b'R' - b'A' + 1;

pub(crate) const DMEXCL: u32 = 0x2000_0000;
pub(crate) const DMAUTH: u32 = 0x0800_0000;
pub(crate) const DMDEVICE: u32 = 0x0080_0000;
pub(crate) const DMNAMEDPIPE: u32 = 0x0020_0000;
pub(crate) const DMSOCKET: u32 = 0x0010_0000;

fn main() {
    if let Err(error) = args::run() {
        eprintln!("r9p: {error}");
        std::process::exit(1);
    }
}

pub(crate) fn usage() -> ! {
    args::usage()
}

#[cfg(test)]
mod tests {
    use crate::commands::mutate::split_parent;
    use crate::format::{
        format_attach, format_stat, format_version, hex_decode, hex_encode, mode_string,
    };
    use crate::io::parse_offset;
    use crate::target::split_namespace_path;
    use r9p::qid::DMDIR;
    use r9p::{qid::Qid, stat::Stat};

    #[test]
    fn namespace_paths_split_like_plan9port_service_paths() {
        let (service, path) =
            split_namespace_path("acme/123/body").expect("namespace path should split");
        assert_eq!(service, "acme");
        assert_eq!(path, "123/body");
    }

    #[test]
    fn create_paths_split_parent_and_leaf() {
        let (parent, name) = split_parent("/entries/new.md").expect("path should split");
        assert_eq!(parent, "/entries");
        assert_eq!(name, "new.md");
    }

    #[test]
    fn write_at_offset_parses_as_decimal_count() {
        assert_eq!(parse_offset("42").expect("offset should parse"), 42);
        assert!(parse_offset("four").is_err());
    }

    #[test]
    fn ls_mode_and_stat_formats_follow_plan9port_shape() {
        let stat = Stat::new("entries", Qid::dir(7), DMDIR | 0o755);
        assert_eq!(mode_string(stat.mode), "d-rwxr-xr-x");
        assert!(format_stat(&stat).contains("q (0000000000000007 0 d)"));
    }

    #[test]
    fn version_and_attach_formats_match_vault_operator_shape() {
        assert_eq!(
            format_version(65_536, b"9P2000"),
            "version=9P2000 msize=65536"
        );
        assert_eq!(format_attach(Qid::dir(42)), "attached qid=dir/0/42");
    }

    #[test]
    fn machine_payloads_are_hex_encoded() {
        assert_eq!(hex_encode(b"9P2000"), "395032303030");
        assert_eq!(
            hex_decode("7661756c74").expect("hex should decode"),
            b"vault"
        );
        assert!(hex_decode("abc").is_err());
    }
}
