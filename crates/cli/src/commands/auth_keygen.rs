use crate::{
    errors::{cli_error, CliResult},
    target::Config,
};
use std::path::PathBuf;

pub(crate) fn auth_keygen_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if config.address.is_some() || config.auth_config.is_some() {
        return Err(cli_error(
            "auth-keygen does not accept an endpoint or auth config",
        ));
    }
    let mut private_path = None;
    let mut public_path = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--private" => {
                index += 1;
                let path = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing private key path"))?,
                );
                if private_path.replace(path).is_some() {
                    return Err(cli_error("private key path already specified"));
                }
            }
            "--public" => {
                index += 1;
                let path = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing public key path"))?,
                );
                if public_path.replace(path).is_some() {
                    return Err(cli_error("public key path already specified"));
                }
            }
            "-h" | "--help" => usage(0),
            other => return Err(cli_error(format!("unknown auth-keygen option {other}"))),
        }
        index += 1;
    }
    let private_path = private_path.ok_or_else(|| cli_error("missing --private path"))?;
    let public_path = public_path.ok_or_else(|| cli_error("missing --public path"))?;
    let pair = r9p_auth::provision_key_pair(&private_path, &public_path)?;
    println!("public_key\t{}", pair.public);
    Ok(())
}

fn usage(code: i32) -> ! {
    eprintln!("usage: r9p auth-keygen --private path --public path");
    std::process::exit(code);
}
