use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    commands::serve::{export_with_config, parse_export_config},
    errors::{cli_error, CliResult},
    target::Config,
};

const DEFAULT_BUNDLE_PATH: &str = ".vault/source-export.bundle";
const DEFAULT_BUNDLE_NAMESPACE_PATH: &str = "/.vault/source-export.bundle";

pub(crate) fn git_export_cmd(global: Config, args: Vec<String>) -> CliResult<()> {
    let config = parse_git_export_config(global, args)?;
    let source = prepare_git_export_source(&config.git)?;
    let mut export_args = config.export_args;
    export_args.push("--descriptor-field".to_string());
    export_args.push(format!(
        "git_bundle_path={}",
        config.git.bundle_namespace_path
    ));
    export_args.push(source.worktree.display().to_string());
    let export_config = parse_export_config(config.global, export_args)?;
    export_with_config(export_config)
}

#[derive(Debug, Clone)]
struct GitExportCommand {
    global: Config,
    git: GitExportConfig,
    export_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitExportConfig {
    repo: PathBuf,
    rev: String,
    worktree: Option<PathBuf>,
    bundle_path: PathBuf,
    bundle_namespace_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedGitSource {
    worktree: PathBuf,
}

fn parse_git_export_config(global: Config, args: Vec<String>) -> CliResult<GitExportCommand> {
    if global.address.is_some() {
        return Err(cli_error(
            "r9p export git uses --bind for its listen address; do not use global -a",
        ));
    }

    let mut repo = PathBuf::from(".");
    let mut rev = "HEAD".to_string();
    let mut worktree = None;
    let mut bundle_path = PathBuf::from(DEFAULT_BUNDLE_PATH);
    let mut bundle_namespace_path = DEFAULT_BUNDLE_NAMESPACE_PATH.to_string();
    let mut export_args = Vec::new();
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                index += 1;
                repo = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing repository path"))?,
                );
            }
            "--rev" => {
                index += 1;
                rev = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing git revision"))?
                    .clone();
            }
            "--worktree" => {
                index += 1;
                worktree = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing worktree path"))?,
                ));
            }
            "--bundle-path" => {
                index += 1;
                bundle_path = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing bundle path"))?,
                );
            }
            "--bundle-namespace-path" => {
                index += 1;
                bundle_namespace_path = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing bundle namespace path"))?
                    .clone();
            }
            "--bind" | "--max-fids" | "--descriptor" | "--descriptor-file" | "--auth"
            | "--descriptor-field" => {
                let option = args[index].clone();
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error(format!("missing value for {option}")))?
                    .clone();
                export_args.push(option);
                export_args.push(value);
            }
            "-h" | "--help" => git_export_usage(0),
            arg if arg.starts_with('-') => {
                return Err(cli_error(format!("unknown export git option {arg}")));
            }
            arg => {
                return Err(cli_error(format!(
                    "unexpected export git argument {arg}: source root is derived from --repo and --rev"
                )));
            }
        }
        index += 1;
    }

    Ok(GitExportCommand {
        global,
        git: GitExportConfig {
            repo,
            rev,
            worktree,
            bundle_path,
            bundle_namespace_path,
        },
        export_args,
    })
}

fn prepare_git_export_source(config: &GitExportConfig) -> CliResult<PreparedGitSource> {
    let revision = resolve_git_revision(&config.repo, &config.rev)?;
    let worktree = config
        .worktree
        .clone()
        .unwrap_or_else(|| default_worktree_path(&config.repo, &revision));
    ensure_clean_worktree(&config.repo, &worktree, &revision)?;
    let bundle_path = bundle_disk_path(&worktree, &config.bundle_path);
    if let Some(parent) = bundle_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            cli_error(format!(
                "create bundle parent {}: {error}",
                parent.display()
            ))
        })?;
    }
    let bundle_arg = path_arg(&bundle_path)?;
    run_git(
        &worktree,
        &["bundle", "create", bundle_arg.as_str(), "--all"],
    )?;
    Ok(PreparedGitSource { worktree })
}

fn resolve_git_revision(repo: &Path, rev: &str) -> CliResult<String> {
    let commit = format!("{rev}^{{commit}}");
    git_output(repo, &["rev-parse", "--verify", &commit]).map(|output| output.trim().to_string())
}

fn ensure_clean_worktree(repo: &Path, worktree: &Path, revision: &str) -> CliResult<()> {
    if worktree.exists() {
        match git_output(worktree, &["rev-parse", "HEAD"]) {
            Ok(output) if output.trim() == revision => reset_worktree(worktree),
            Ok(_) => replace_existing_worktree(repo, worktree, revision),
            Err(_) if managed_default_worktree(worktree) => {
                std::fs::remove_dir_all(worktree).map_err(|error| {
                    cli_error(format!(
                        "remove stale generated worktree {}: {error}",
                        worktree.display()
                    ))
                })?;
                add_worktree(repo, worktree, revision)
            }
            Err(_) => Err(cli_error(format!(
                "worktree path {} exists but is not a git worktree",
                worktree.display()
            ))),
        }
    } else {
        add_worktree(repo, worktree, revision)
    }
}

fn reset_worktree(worktree: &Path) -> CliResult<()> {
    run_git(worktree, &["reset", "--hard"])?;
    run_git(worktree, &["clean", "-fd", "-e", ".vault"])
}

fn replace_existing_worktree(repo: &Path, worktree: &Path, revision: &str) -> CliResult<()> {
    let worktree_arg = path_arg(worktree)?;
    let _ignored = run_git(
        repo,
        &["worktree", "remove", "--force", worktree_arg.as_str()],
    );
    if worktree.exists() {
        if managed_default_worktree(worktree) {
            std::fs::remove_dir_all(worktree).map_err(|error| {
                cli_error(format!(
                    "remove generated worktree {}: {error}",
                    worktree.display()
                ))
            })?;
        } else {
            return Err(cli_error(format!(
                "worktree path {} already exists at another revision",
                worktree.display()
            )));
        }
    }
    add_worktree(repo, worktree, revision)
}

fn add_worktree(repo: &Path, worktree: &Path, revision: &str) -> CliResult<()> {
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            cli_error(format!(
                "create worktree parent {}: {error}",
                parent.display()
            ))
        })?;
    }
    let worktree_arg = path_arg(worktree)?;
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--force",
            "--detach",
            worktree_arg.as_str(),
            revision,
        ],
    )
}

fn default_worktree_path(repo: &Path, revision: &str) -> PathBuf {
    let repo_name = repo
        .canonicalize()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_os_string()))
        .or_else(|| repo.file_name().map(|name| name.to_os_string()))
        .and_then(|name| name.into_string().ok())
        .unwrap_or_else(|| "repo".to_string());
    env::temp_dir().join(format!(
        "r9p-git-source-{}-{}",
        safe_token(&repo_name),
        revision_token(revision)
    ))
}

fn managed_default_worktree(path: &Path) -> bool {
    let temp_dir = env::temp_dir();
    path.parent() == Some(temp_dir.as_path())
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("r9p-git-source-"))
            .unwrap_or(false)
}

fn bundle_disk_path(worktree: &Path, bundle_path: &Path) -> PathBuf {
    if bundle_path.is_absolute() {
        bundle_path.to_path_buf()
    } else {
        worktree.join(bundle_path)
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> CliResult<()> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|error| cli_error(format!("run git in {}: {error}", cwd.display())))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(cli_error(format!(
            "git in {} failed: {}",
            cwd.display(),
            command_failure(&output)
        )))
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> CliResult<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|error| cli_error(format!("run git in {}: {error}", cwd.display())))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| cli_error(format!("git output was not utf-8: {error}")))
    } else {
        Err(cli_error(format!(
            "git in {} failed: {}",
            cwd.display(),
            command_failure(&output)
        )))
    }
}

fn command_failure(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    format!("status={} {details}", output.status)
}

fn path_arg(path: &Path) -> CliResult<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| cli_error(format!("path {} is not utf-8", path.display())))
}

fn revision_token(revision: &str) -> String {
    safe_token(revision).chars().take(12).collect()
}

fn safe_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn git_export_usage(code: i32) -> ! {
    eprintln!(
        "usage: r9p export git [--repo path] [--rev rev] [--worktree path] [--bundle-path path] [--bundle-namespace-path path] [--bind address] [--max-fids count] [--descriptor-file path] [--auth boundary]"
    );
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{target::Config, DEFAULT_MSIZE};

    fn global() -> Config {
        Config {
            address: None,
            aname: String::new(),
            uname: "codex".to_string(),
            msize: DEFAULT_MSIZE,
            msize_set: false,
            machine: false,
            request_timeout: Some(std::time::Duration::from_secs(30)),
        }
    }

    #[test]
    fn parses_git_and_export_options() {
        let config = parse_git_export_config(
            global(),
            vec![
                "--repo".to_string(),
                "/tmp/repo".to_string(),
                "--rev".to_string(),
                "main".to_string(),
                "--worktree".to_string(),
                "/tmp/worktree".to_string(),
                "--bundle-path".to_string(),
                ".vault/source.bundle".to_string(),
                "--bundle-namespace-path".to_string(),
                "/.vault/source.bundle".to_string(),
                "--bind".to_string(),
                "127.0.0.1:0".to_string(),
                "--descriptor-file".to_string(),
                "/tmp/source.desc".to_string(),
            ],
        )
        .expect("config should parse");

        assert_eq!(config.git.repo, PathBuf::from("/tmp/repo"));
        assert_eq!(config.git.rev, "main");
        assert_eq!(config.git.worktree, Some(PathBuf::from("/tmp/worktree")));
        assert_eq!(
            config.git.bundle_path,
            PathBuf::from(".vault/source.bundle")
        );
        assert_eq!(config.git.bundle_namespace_path, "/.vault/source.bundle");
        assert_eq!(
            config.export_args,
            vec![
                "--bind".to_string(),
                "127.0.0.1:0".to_string(),
                "--descriptor-file".to_string(),
                "/tmp/source.desc".to_string()
            ]
        );
    }

    #[test]
    fn revision_token_is_path_safe_and_bounded() {
        assert_eq!(revision_token("git:feature/foo_bar"), "git-feature-");
        assert_eq!(revision_token("f1c7932d2d30a09fd0cc"), "f1c7932d2d30");
    }
}
