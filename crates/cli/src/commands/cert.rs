//! `r9p cert` — the offline signer.
//!
//! Deliberately not a service. The root signs identities that live for years,
//! so it belongs in sops and in an operator's hands, not in a daemon that can
//! be reached. Short-lived credentials would need an online issuer; these do
//! not.
//!
//! It consumes `auth-keygen` rather than replacing it: signing happens over a
//! *public* key, so the private half never leaves the host that generated it.
//! `nebula-cert sign` mints the pair itself and ships it, which is weaker.

use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};
use r9p_auth::{
    now_unix, provision_root_key_pair, Certificate, CertificateBody, PublicKey, RootPrivateKey,
    RootPublicKey, UnixSeconds,
};
use std::path::PathBuf;

const SECONDS_PER_DAY: u64 = 86_400;

pub(crate) fn cert_cmd(config: Config, mut args: Vec<String>) -> CliResult<()> {
    if config.address.is_some() || config.auth_config.is_some() {
        return Err(cli_error("cert does not accept an endpoint or auth config"));
    }
    if args.is_empty() {
        usage(2);
    }
    let subcommand = args.remove(0);
    match subcommand.as_str() {
        "root" => root_cmd(args),
        "sign" => sign_cmd(args),
        "print" => print_cmd(args),
        "verify" => verify_cmd(args),
        "-h" | "--help" => usage(0),
        other => Err(cli_error(format!("unknown cert subcommand {other}"))),
    }
}

fn root_cmd(args: Vec<String>) -> CliResult<()> {
    let mut private_path: Option<PathBuf> = None;
    let mut public_path: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--private" => set_path(&mut private_path, &args, &mut index, "private key path")?,
            "--public" => set_path(&mut public_path, &args, &mut index, "public key path")?,
            "-h" | "--help" => usage(0),
            other => return Err(cli_error(format!("unknown cert root option {other}"))),
        }
        index += 1;
    }
    let private_path = private_path.ok_or_else(|| cli_error("missing --private path"))?;
    let public_path = public_path.ok_or_else(|| cli_error("missing --public path"))?;
    let pair = provision_root_key_pair(&private_path, &public_path)?;
    println!("root_public_key\t{}", pair.public);
    Ok(())
}

fn sign_cmd(args: Vec<String>) -> CliResult<()> {
    let mut root_private: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut key_hex: Option<String> = None;
    let mut key_file: Option<PathBuf> = None;
    let mut groups = Vec::new();
    let mut days: Option<u64> = None;
    let mut not_before: Option<UnixSeconds> = None;
    let mut not_after: Option<UnixSeconds> = None;
    let mut out: Option<PathBuf> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root-private" => set_path(&mut root_private, &args, &mut index, "root private key")?,
            "--name" => set_string(&mut name, &args, &mut index, "name")?,
            "--key" => set_string(&mut key_hex, &args, &mut index, "key")?,
            "--key-file" => set_path(&mut key_file, &args, &mut index, "key file")?,
            "--group" => groups.push(value(&args, &mut index, "group")?),
            "--days" => days = Some(parse_u64(&value(&args, &mut index, "days")?, "days")?),
            "--not-before" => {
                not_before = Some(parse_u64(
                    &value(&args, &mut index, "not-before")?,
                    "not-before",
                )?);
            }
            "--not-after" => {
                not_after = Some(parse_u64(
                    &value(&args, &mut index, "not-after")?,
                    "not-after",
                )?);
            }
            "--out" => set_path(&mut out, &args, &mut index, "output path")?,
            "-h" | "--help" => usage(0),
            other => return Err(cli_error(format!("unknown cert sign option {other}"))),
        }
        index += 1;
    }

    let root_private = root_private.ok_or_else(|| cli_error("missing --root-private path"))?;
    let name = name.ok_or_else(|| cli_error("missing --name"))?;
    let subject = match (key_hex, key_file) {
        (Some(_), Some(_)) => return Err(cli_error("use either --key or --key-file, not both")),
        (Some(hex), None) => PublicKey::from_hex(&hex)?,
        (None, Some(path)) => PublicKey::read(&path)?,
        (None, None) => return Err(cli_error("missing --key or --key-file")),
    };

    let not_before = match not_before {
        Some(value) => value,
        None => now_unix()?,
    };
    let not_after = match (not_after, days) {
        (Some(_), Some(_)) => return Err(cli_error("use either --days or --not-after, not both")),
        (Some(value), None) => value,
        (None, Some(days)) => days
            .checked_mul(SECONDS_PER_DAY)
            .and_then(|span| not_before.checked_add(span))
            .ok_or_else(|| cli_error("--days overflows the validity window"))?,
        (None, None) => return Err(cli_error("missing --days or --not-after")),
    };

    let root = RootPrivateKey::read(&root_private)?;
    let body = CertificateBody::new(name, subject, groups, not_before, not_after, root.public())?;
    let certificate = Certificate::sign(&root, body)?;

    match out {
        // Refuses to clobber, like key provisioning: rotating an identity
        // should be a deliberate removal, not a silent overwrite.
        Some(path) => {
            certificate.write(&path)?;
            println!("certificate\t{}", path.display());
            println!("name\t{}", certificate.body().name());
            println!("not_after\t{}", certificate.body().not_after());
        }
        None => print!("{}", certificate.render()),
    }
    Ok(())
}

fn print_cmd(args: Vec<String>) -> CliResult<()> {
    let mut path: Option<PathBuf> = None;
    let mut at: Option<UnixSeconds> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--path" => set_path(&mut path, &args, &mut index, "certificate path")?,
            "--at" => at = Some(parse_u64(&value(&args, &mut index, "at")?, "at")?),
            "-h" | "--help" => usage(0),
            other => return Err(cli_error(format!("unknown cert print option {other}"))),
        }
        index += 1;
    }
    let path = path.ok_or_else(|| cli_error("missing --path"))?;
    let certificate = Certificate::read(&path)?;
    let body = certificate.body();
    let now = match at {
        Some(value) => value,
        None => now_unix()?,
    };

    println!("name\t{}", body.name());
    println!("key\t{}", body.key());
    for group in body.groups() {
        println!("group\t{group}");
    }
    println!("not_before\t{}", body.not_before());
    println!("not_after\t{}", body.not_after());
    println!("issuer\t{}", body.issuer());
    // Remaining seconds rather than a formatted date: it needs no calendar in
    // the trust path, and "how long is left" is the form a threshold alert
    // actually wants. Mirrors the mesh service's certificate_ttl_seconds.
    let remaining = i128::from(body.not_after()) - i128::from(now);
    println!("expires_in_seconds\t{remaining}");
    Ok(())
}

fn verify_cmd(args: Vec<String>) -> CliResult<()> {
    let mut path: Option<PathBuf> = None;
    let mut root_hex: Option<String> = None;
    let mut root_file: Option<PathBuf> = None;
    let mut at: Option<UnixSeconds> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--path" => set_path(&mut path, &args, &mut index, "certificate path")?,
            "--root" => set_string(&mut root_hex, &args, &mut index, "root")?,
            "--root-file" => set_path(&mut root_file, &args, &mut index, "root file")?,
            "--at" => at = Some(parse_u64(&value(&args, &mut index, "at")?, "at")?),
            "-h" | "--help" => usage(0),
            other => return Err(cli_error(format!("unknown cert verify option {other}"))),
        }
        index += 1;
    }
    let path = path.ok_or_else(|| cli_error("missing --path"))?;
    let root = match (root_hex, root_file) {
        (Some(_), Some(_)) => return Err(cli_error("use either --root or --root-file, not both")),
        (Some(hex), None) => RootPublicKey::from_hex(&hex)?,
        (None, Some(file)) => RootPublicKey::read(&file)?,
        (None, None) => return Err(cli_error("missing --root or --root-file")),
    };
    let now = match at {
        Some(value) => value,
        None => now_unix()?,
    };
    let certificate = Certificate::read(&path)?;
    certificate.verify(root, now)?;
    println!("verified\t{}", certificate.body().name());
    Ok(())
}

fn value(args: &[String], index: &mut usize, field: &str) -> CliResult<String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| cli_error(format!("missing {field}")))
}

fn set_string(
    target: &mut Option<String>,
    args: &[String],
    index: &mut usize,
    field: &str,
) -> CliResult<()> {
    let parsed = value(args, index, field)?;
    if target.replace(parsed).is_some() {
        return Err(cli_error(format!("{field} already specified")));
    }
    Ok(())
}

fn set_path(
    target: &mut Option<PathBuf>,
    args: &[String],
    index: &mut usize,
    field: &str,
) -> CliResult<()> {
    let parsed = PathBuf::from(value(args, index, field)?);
    if target.replace(parsed).is_some() {
        return Err(cli_error(format!("{field} already specified")));
    }
    Ok(())
}

fn parse_u64(value: &str, field: &str) -> CliResult<u64> {
    value
        .parse::<u64>()
        .map_err(|_| cli_error(format!("{field} must be a whole number")))
}

fn usage(code: i32) -> ! {
    eprintln!("usage: r9p cert root --private path --public path");
    eprintln!("       r9p cert sign --root-private path --name name (--key hex | --key-file path)");
    eprintln!("                     [--group name]... (--days n | --not-after seconds)");
    eprintln!("                     [--not-before seconds] [--out path]");
    eprintln!("       r9p cert print --path path [--at seconds]");
    eprintln!("       r9p cert verify --path path (--root hex | --root-file path) [--at seconds]");
    std::process::exit(code);
}
