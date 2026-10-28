//! What delivering a change needs, and the server's backend key: the release selection that
//! the Deploy Images workflow and `release:plan` share. It builds without the `web` feature,
//! so the workflow compiles only this and its small dependencies.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{Args, run_output};

const BACKEND_PATHS: [&str; 4] = ["Cargo.toml", "Cargo.lock", "shared", "server"];
pub const ALL_SERVICES: [&str; 3] = ["server", "ingest", "edge"];

/// `xtask release plan [--base REV] [--head REV]` and `xtask release key [--commit REV]`.
pub fn command(root: &Path, name: &str, args: &[String]) -> Result<()> {
    let args = Args::parse(args, &["base", "head", "commit"], &[])?;
    ensure!(
        args.positional.is_empty(),
        "unexpected argument {:?}",
        args.positional
    );
    let git = Git(root.to_path_buf());
    match name {
        "plan" => {
            let base = git.resolve(args.value("base").unwrap_or("origin/main"))?;
            let head = git.resolve(args.value("head").unwrap_or("HEAD"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&plan(&git, &base, &head)?)?
            );
        }
        "key" => {
            let commit = git.resolve(args.value("commit").unwrap_or("HEAD"))?;
            println!("{}", git.backend_key(&commit)?);
        }
        other => bail!("unknown selection command {other}"),
    }
    Ok(())
}

/// What a static release can deliver on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Static {
    /// Every byte of the live release except its stylesheet.
    Css,
    /// Both clients rebuilt.
    Client,
}

/// What a changed path needs: the static release that can deliver it (none when only an image
/// can), the images a normal deploy rebuilds and whether the platform configuration changes.
/// Server images embed both clients, so client paths also name the server image.
struct Rule {
    pattern: &'static str,
    release: Option<Static>,
    pub images: &'static [&'static str],
    pub config: bool,
}

const fn rule(
    pattern: &'static str,
    release: Option<Static>,
    images: &'static [&'static str],
    config: bool,
) -> Rule {
    Rule {
        pattern,
        release,
        images,
        config,
    }
}

const SERVER_IMAGE: &[&str] = &["server"];
const NOTHING: &[&str] = &[];

/// First match wins; a trailing `/*` matches everything below. A path no rule names needs
/// everything.
const RULES: &[Rule] = &[
    rule("client/input.css", Some(Static::Css), SERVER_IMAGE, false),
    rule("client/*", Some(Static::Client), SERVER_IMAGE, false),
    rule("claims-client/*", Some(Static::Client), SERVER_IMAGE, false),
    rule("browser-map/*", Some(Static::Client), SERVER_IMAGE, false),
    rule("wasm/*", Some(Static::Client), SERVER_IMAGE, false),
    // The web build is the only part of the tool that shapes an artifact: the clients.
    rule(
        "tools/xtask/src/web.rs",
        Some(Static::Client),
        SERVER_IMAGE,
        false,
    ),
    rule(
        "tools/xtask/Cargo.toml",
        Some(Static::Client),
        SERVER_IMAGE,
        false,
    ),
    rule(
        "tools/xtask/Cargo.lock",
        Some(Static::Client),
        SERVER_IMAGE,
        false,
    ),
    rule("tools/*", None, NOTHING, false),
    rule("Cargo.toml", None, &["server", "ingest"], false),
    rule("shared/*", None, &["server", "ingest"], false),
    rule("Cargo.lock", None, SERVER_IMAGE, false),
    rule("Dockerfile", None, SERVER_IMAGE, false),
    rule(".ci/*", None, SERVER_IMAGE, false),
    rule("server/*", None, SERVER_IMAGE, false),
    rule("services/sequoia-ingest/*", None, &["ingest"], false),
    rule("ops/caddy/*", None, &["edge"], false),
    rule(".dockerignore", None, &ALL_SERVICES, false),
    rule("compose.yaml", None, &ALL_SERVICES, true),
    rule(
        ".github/workflows/deploy-images.yml",
        None,
        &ALL_SERVICES,
        false,
    ),
    rule("README.md", None, NOTHING, false),
    rule("AGENTS.md", None, NOTHING, false),
    rule("LICENSE", None, NOTHING, false),
    rule("docs/*", None, NOTHING, false),
    rule(".gitignore", None, NOTHING, false),
    rule(".cargo/*", None, NOTHING, false),
    rule("mise.toml", None, NOTHING, false),
    rule("mise.lock", None, NOTHING, false),
    rule(".github/workflows/ci.yml", None, NOTHING, false),
    rule(".github/actionlint.yaml", None, NOTHING, false),
    rule("scripts/*", None, NOTHING, false),
    rule("ops/backup/*", None, NOTHING, false),
    rule("ops/prometheus/*", None, NOTHING, false),
];

fn rule_for(path: &str) -> Option<&'static Rule> {
    RULES
        .iter()
        .find(|rule| match rule.pattern.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == rule.pattern,
        })
}

/// What delivering a change to `paths` needs.
#[derive(Debug, serde::Serialize)]
pub struct Needs {
    pub paths: Vec<String>,
    pub unknown: Vec<String>,
    pub client_release: Option<Static>,
    images: Vec<&'static str>,
    config: bool,
    pub static_release_suffices: bool,
}

pub fn classify(paths: Vec<String>) -> Needs {
    let rules: Vec<_> = paths.iter().map(|path| rule_for(path)).collect();
    let unknown = paths
        .iter()
        .zip(&rules)
        .filter(|(_, rule)| rule.is_none())
        .map(|(path, _)| path.clone())
        .collect();
    let client_release = rules.iter().flatten().filter_map(|rule| rule.release).max();
    let images = ALL_SERVICES
        .into_iter()
        .filter(|service| {
            rules
                .iter()
                .any(|rule| rule.is_none_or(|rule| rule.images.contains(service)))
        })
        .collect();
    // A static release suffices when every path is a client path or needs nothing at all.
    let static_release_suffices = client_release.is_some()
        && rules.iter().all(|rule| {
            rule.is_some_and(|rule| {
                rule.release.is_some() || (rule.images.is_empty() && !rule.config)
            })
        });
    Needs {
        paths,
        unknown,
        client_release,
        images,
        config: rules.iter().any(|rule| rule.is_none_or(|rule| rule.config)),
        static_release_suffices,
    }
}

pub fn plan(git: &Git, base: &str, head: &str) -> Result<Value> {
    let mut needs = classify(git.changed_paths(base, head)?);
    needs.static_release_suffices &= git.backend_key(base)? == git.backend_key(head)?;
    let mut plan = serde_json::to_value(needs)?;
    plan["base"] = json!(base);
    plan["head"] = json!(head);
    Ok(plan)
}

pub struct Git(pub PathBuf);

impl Git {
    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        run_output(Command::new("git").arg("-C").arg(&self.0).args(args))
    }

    pub fn text(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8(self.run(args)?)?.trim().to_owned())
    }

    pub fn resolve(&self, rev: &str) -> Result<String> {
        self.text(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
    }

    pub fn changed_paths(&self, base: &str, head: &str) -> Result<Vec<String>> {
        Ok(self
            .text(&["diff", "--name-only", "--no-renames", base, head])?
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// The server's backend key for `commit`, as the image build computes it: the first 16
    /// hex digits of the SHA-256 of `sha256sum` lines for every backend file in byte order.
    pub fn backend_key(&self, commit: &str) -> Result<String> {
        let mut args = vec!["ls-tree", "-r", "-z", commit, "--"];
        args.extend(BACKEND_PATHS);
        let listing = self.run(&args)?;
        let mut entries: Vec<(&[u8], &[u8])> = listing
            .split(|&byte| byte == 0)
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let tab = line.iter().position(|&byte| byte == b'\t')?;
                let mut meta = line[..tab].split(|&byte| byte == b' ');
                let (mode, kind, oid) = (meta.next()?, meta.next()?, meta.next()?);
                (kind == b"blob" && mode != b"120000").then_some((&line[tab + 1..], oid))
            })
            .collect();
        entries.sort();
        let mut batch = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let request: Vec<u8> = entries
            .iter()
            .flat_map(|(_, oid)| oid.iter().copied().chain([b'\n']))
            .collect();
        let mut stdin = batch.stdin.take().context("git cat-file stdin")?;
        let writer = std::thread::spawn(move || stdin.write_all(&request));
        let output = batch.wait_with_output()?;
        writer.join().expect("cat-file writer")?;
        ensure!(output.status.success(), "git cat-file failed");
        let mut lines = Vec::new();
        let mut rest = output.stdout.as_slice();
        for (path, _) in &entries {
            let header_end = rest
                .iter()
                .position(|&byte| byte == b'\n')
                .context("cat-file header")?;
            let size: usize = std::str::from_utf8(&rest[..header_end])?
                .rsplit(' ')
                .next()
                .context("cat-file size")?
                .parse()?;
            let content = &rest[header_end + 1..header_end + 1 + size];
            rest = &rest[header_end + 1 + size + 1..];
            lines.extend_from_slice(hex(&Sha256::digest(content)).as_bytes());
            lines.extend_from_slice(b"  ");
            lines.extend_from_slice(path);
            lines.push(b'\n');
        }
        Ok(hex(&Sha256::digest(&lines))[..16].to_owned())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn needs(paths: &[&str]) -> Needs {
        classify(paths.iter().map(|path| (*path).to_owned()).collect())
    }

    #[test]
    fn deliveries_follow_what_each_path_feeds() {
        let css = needs(&["client/input.css", "docs/deployment.md"]);
        assert_eq!(css.client_release, Some(Static::Css));
        assert!(css.static_release_suffices);
        assert_eq!(css.images, ["server"]);

        let client = needs(&["client/input.css", "browser-map/src/lib.rs"]);
        assert_eq!(client.client_release, Some(Static::Client));
        assert!(client.static_release_suffices);

        let mixed = needs(&["client/src/app.rs", "server/src/main.rs"]);
        assert!(!mixed.static_release_suffices);
        assert_eq!(mixed.images, ["server"]);

        let unknown = needs(&["new-top-level-file"]);
        assert_eq!(unknown.images, ALL_SERVICES);
        assert!(unknown.config && !unknown.static_release_suffices);

        let manifest = needs(&["compose.yaml"]);
        assert!(manifest.unknown.is_empty() && manifest.config);
        assert_eq!(manifest.images, ALL_SERVICES);

        let docs = needs(&["README.md", "mise.toml"]);
        assert!(docs.images.is_empty() && docs.client_release.is_none());
    }

    #[test]
    fn tool_changes_select_nothing_unless_they_change_the_client_build() {
        let tool = needs(&["tools/xtask/src/release.rs", "tools/xtask/src/watch.rs"]);
        assert!(tool.images.is_empty() && tool.client_release.is_none() && tool.unknown.is_empty());

        let build = needs(&["tools/xtask/src/web.rs"]);
        assert_eq!(build.client_release, Some(Static::Client));
        assert!(build.static_release_suffices);
    }

    #[test]
    fn backend_key_matches_the_image_builds_sha256sum_pipeline() {
        let repo = std::env::temp_dir().join(format!("xtask-key-{}", std::process::id()));
        let _ = fs::remove_dir_all(&repo);
        for (path, content) in [
            ("Cargo.toml", "[workspace]\n"),
            ("Cargo.lock", "lock\n"),
            ("shared/src/lib.rs", "pub fn shared() {}\n"),
            ("server/src/main.rs", "fn main() {}\n"),
            ("server/static/a b.html", "<p>space</p>\n"),
            ("client/src/main.rs", "not backend\n"),
        ] {
            fs::create_dir_all(repo.join(path).parent().expect("parent")).expect("mkdir");
            fs::write(repo.join(path), content).expect("write");
        }
        let git = |args: &[&str]| {
            run_output(Command::new("git").arg("-C").arg(&repo).args(args)).map(drop)
        };
        git(&["init", "-q"]).expect("init");
        git(&["add", "-A"]).expect("add");
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "t",
        ])
        .expect("commit");
        let image_key = run_output(Command::new("sh").current_dir(&repo).arg("-c").arg(
            "find Cargo.toml Cargo.lock shared server -type f -print0 | LC_ALL=C sort -z \
             | xargs -0 sha256sum | sha256sum | cut -c1-16",
        ))
        .expect("pipeline");
        let key = Git(repo.clone()).backend_key("HEAD").expect("key");
        fs::remove_dir_all(&repo).expect("cleanup");
        assert_eq!(key, String::from_utf8(image_key).expect("utf8").trim());
    }
}
