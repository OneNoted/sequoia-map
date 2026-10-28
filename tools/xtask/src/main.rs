//! Repository commands: the browser-client build, the development watcher and releases.

mod plan;
#[cfg(feature = "web")]
mod release;
#[cfg(feature = "web")]
mod watch;
#[cfg(feature = "web")]
mod web;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

const USAGE: &str = "usage:
  xtask web [--release] [map|claims]...   build browser clients into <client>/dist
  xtask watch                             rebuild on change and reload browsers (dev daemon)
  xtask release plan [--base REV] [--head REV]
  xtask release key [--commit REV]
  xtask release client [--base-release DIR | --from-target] [--out DIR]
  xtask release activate BUNDLE [--dry-run]
  xtask release status
  xtask release rollback [--dry-run]
  xtask release services SERVICE... [--dry-run]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let root = repo_root()?;
    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("release"), Some(name @ ("plan" | "key"))) => plan::command(&root, name, &args[2..]),
        #[cfg(feature = "web")]
        (Some("web"), _) => web::command(&root, &args[1..]),
        #[cfg(feature = "web")]
        (Some("watch"), _) => watch::command(&root, &args[1..]),
        #[cfg(feature = "web")]
        (Some("release"), _) => release::command(&root, &args[1..]),
        #[cfg(not(feature = "web"))]
        (Some("web" | "watch" | "release"), _) => {
            bail!("this xtask was built without the web feature")
        }
        _ => bail!("{USAGE}"),
    }
}

/// The repository root: two levels above this crate.
fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .context("resolve the repository root")
}

/// Runs `command`, failing with its name when it cannot start or exits unsuccessfully.
#[cfg(feature = "web")]
fn run_status(command: &mut Command) -> Result<()> {
    let name = command.get_program().to_string_lossy().into_owned();
    let status = command.status().with_context(|| format!("start {name}"))?;
    if !status.success() {
        bail!("{name} failed ({status})");
    }
    Ok(())
}

/// Runs a build `command`, echoing its stderr; when it fails, the error carries the build's
/// own diagnostics (from its first error), which the watcher shows in the page.
#[cfg(feature = "web")]
fn run_diagnosed(command: &mut Command) -> Result<()> {
    use std::io::BufRead;

    let name = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("start {name}"))?;
    let stderr = child.stderr.take().context("build stderr")?;
    let mut lines = Vec::new();
    for line in std::io::BufReader::new(stderr).lines() {
        let line = line?;
        eprintln!("{line}");
        lines.push(line);
    }
    let status = child.wait()?;
    if status.success() {
        return Ok(());
    }
    let first_error = lines
        .iter()
        .position(|line| line.starts_with("error") || line.contains("Error"))
        .unwrap_or(lines.len().saturating_sub(40));
    let diagnostics: Vec<&str> = lines[first_error..]
        .iter()
        .take(200)
        .map(String::as_str)
        .collect();
    bail!("{name} failed ({status})\n{}", diagnostics.join("\n"));
}

/// Runs `command` and returns its stdout, with its stderr in the error when it fails.
fn run_output(command: &mut Command) -> Result<Vec<u8>> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command.output().with_context(|| format!("start {name}"))?;
    if !output.status.success() {
        bail!(
            "{name} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// Splits `args` into flags (`--name` or `--name value`, by `valued`) and positionals.
struct Args {
    flags: Vec<(String, Option<String>)>,
    positional: Vec<String>,
}

impl Args {
    fn parse(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Self> {
        let mut flags = Vec::new();
        let mut positional = Vec::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.strip_prefix("--") {
                Some(name) if valued.contains(&name) => {
                    let value = iter
                        .next()
                        .with_context(|| format!("--{name} needs a value"))?;
                    flags.push((name.to_owned(), Some(value.clone())));
                }
                Some(name) if switches.contains(&name) => flags.push((name.to_owned(), None)),
                Some(name) => bail!("unknown option --{name}"),
                None => positional.push(arg.clone()),
            }
        }
        Ok(Self { flags, positional })
    }

    #[cfg(feature = "web")]
    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(flag, _)| flag == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(flag, _)| flag == name)
            .and_then(|(_, value)| value.as_deref())
    }
}
