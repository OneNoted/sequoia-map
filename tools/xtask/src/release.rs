//! Releases: what a change needs, client release bundles, and their activation, status and
//! rollback on the server, plus selective service image updates through Compose.
//!
//! The target is the server container: `SEQUOIA_RELEASE_EXEC` is the command prefix that runs
//! a command in it (for example `ssh host docker exec -i <container>`) and
//! `SEQUOIA_RELEASE_URL` is its public base URL. `SEQUOIA_COMPOSE_EXEC` is the
//! `docker compose` prefix for the deployed project and its existing configuration, which this
//! tool never reads. Every stage appends a timed receipt to `.data/releases/receipts.jsonl`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::plan::{ALL_SERVICES, Git, Static, classify, hex};
use crate::web::{self, CLIENTS, Profile};
use crate::{Args, run_output, run_status};

const SERVER: &str = "/app/sequoia-server";
/// Each client's directory in a release, and the URL path its page is checked at.
const CLIENT_PAGES: [(&str, &str, &str); 2] = [
    ("client/dist", "/", "/"),
    ("claims-client/dist", "/claims/new", "/claims-app/"),
];
/// The public edge refuses requests without a named client.
const USER_AGENT: &str = "SequoiaMapRelease/1.0 (+https://github.com/OneNoted/sequoia-map)";

pub fn command(root: &Path, args: &[String]) -> Result<()> {
    let (name, rest) = args.split_first().context("release needs a subcommand")?;
    let args = Args::parse(
        rest,
        &["base-release", "out", "exec", "url", "compose"],
        &["from-target", "dry-run"],
    )?;
    let git = Git(root.to_path_buf());
    match name.as_str() {
        "client" => command_client(root, &git, &args),
        "activate" => command_activate(&args),
        "status" => print_json(&Target::new(&args)?.status()?),
        "rollback" => command_rollback(&args),
        "services" => command_services(&args),
        other => bail!("unknown release subcommand {other}"),
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Whether delivering `head` over the commit a release was built from needs only a new
/// stylesheet, and if not, why.
fn css_only(git: &Git, base_source: Option<&str>, head: &str) -> (bool, Option<String>) {
    let Some(base_source) = base_source.filter(|source| !source.is_empty()) else {
        return (
            false,
            Some("the base release records no source commit".into()),
        );
    };
    let Ok(base) = git.resolve(base_source) else {
        return (
            false,
            Some(format!(
                "the base release's commit {base_source} is not in this repository"
            )),
        );
    };
    match git.changed_paths(&base, head).map(classify) {
        Ok(needs) if needs.client_release == Some(Static::Css) && needs.static_release_suffices => {
            (true, None)
        }
        _ => (
            false,
            Some("the change since the base release needs more than a stylesheet".into()),
        ),
    }
}

/// The image's version of a build tool, from its Dockerfile.
fn dockerfile_version(root: &Path, name: &str) -> Result<String> {
    let dockerfile = fs::read_to_string(root.join("Dockerfile"))?;
    dockerfile
        .lines()
        .find_map(|line| line.strip_prefix(&format!("ARG {name}=")))
        .map(str::to_owned)
        .with_context(|| format!("the Dockerfile has no {name}"))
}

/// A local release must use the image's Tailwind and Binaryen, or its bytes would differ from
/// what the image builds.
fn require_image_tools(root: &Path) -> Result<()> {
    for (command, flag, arg, expected) in [
        ("tailwindcss", "--help", "TAILWINDCSS_VERSION", "v"),
        ("wasm-opt", "--version", "BINARYEN_VERSION", "version "),
    ] {
        let expected = format!("{expected}{}", dockerfile_version(root, arg)?);
        let output = Command::new(command)
            .arg(flag)
            .output()
            .with_context(|| format!("run {command}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            text.contains(&expected),
            "{command} must be {expected}, the image's version; it reports: {}",
            text.lines().next().unwrap_or_default()
        );
    }
    Ok(())
}

fn file_digests(root: &Path) -> Result<BTreeMap<String, Value>> {
    let mut files = Vec::new();
    web::collect_files(root, &mut files)?;
    files
        .into_iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(root).ok()?.to_str()?.to_owned();
            (relative != "release.json").then_some((relative, path))
        })
        .map(|(relative, path)| {
            let data = fs::read(&path)?;
            Ok((
                relative,
                json!({ "sha256": hex(&Sha256::digest(&data)), "size": data.len() }),
            ))
        })
        .collect()
}

/// Reuses every file of `base` and replaces only the map's compiled stylesheet.
fn build_css_release(root: &Path, base: &Path, bundle: &Path) -> Result<Value> {
    copy_dir(base, bundle)?;
    let _ = fs::remove_file(bundle.join("release.json"));
    let dist = bundle.join("client/dist");
    let page = fs::read_to_string(dist.join("index.html"))?;
    let css = web::compile_tailwind(&root.join("client"), &bundle.with_extension("tailwind.css"))?;
    let new_name = web::stylesheet_name(&css);
    let (html, old_name) = swap_stylesheet(&page, &new_name, &web::sri(&css))?;
    for suffix in ["", ".br", ".gz"] {
        let _ = fs::remove_file(dist.join(format!("{old_name}{suffix}")));
    }
    fs::write(dist.join(&new_name), &css)?;
    fs::write(dist.join("index.html"), &html)?;
    for (name, data) in [
        (new_name.as_str(), css.as_slice()),
        ("index.html", html.as_bytes()),
    ] {
        fs::write(dist.join(format!("{name}.br")), web::brotli(data)?)?;
        fs::write(dist.join(format!("{name}.gz")), web::gzip(data)?)?;
    }
    let (before, after) = (file_digests(base)?, file_digests(bundle)?);
    let touched: Vec<String> = [old_name.as_str(), new_name.as_str(), "index.html"]
        .iter()
        .flat_map(|name| ["", ".br", ".gz"].map(|suffix| format!("client/dist/{name}{suffix}")))
        .collect();
    let reused: Vec<_> = after
        .keys()
        .filter(|path| !touched.contains(path))
        .collect();
    ensure!(
        reused
            .iter()
            .all(|path| before.get(*path) == after.get(*path)),
        "a CSS-only release must reuse every other file byte for byte"
    );
    Ok(json!({ "stylesheet": new_name, "replaced": old_name, "reused_files": reused.len() }))
}

/// Points the page's one stylesheet link at `name` with `integrity`; returns the page and the
/// replaced stylesheet's name.
fn swap_stylesheet(page: &str, name: &str, integrity: &str) -> Result<(String, String)> {
    let tags: Vec<&str> = tags(page)
        .into_iter()
        .filter(|tag| tag.starts_with("<link") && tag.contains("rel=\"stylesheet\""))
        .collect();
    ensure!(
        tags.len() == 1,
        "the base release's map page must link exactly one stylesheet"
    );
    let tag = tags[0];
    let old_href = attribute(tag, "href").context("the stylesheet link has no href")?;
    let old_integrity =
        attribute(tag, "integrity").context("the stylesheet link has no integrity")?;
    let old_name = old_href.trim_start_matches('/').to_owned();
    let new_tag = tag
        .replace(old_href, &format!("/{name}"))
        .replace(old_integrity, integrity);
    Ok((page.replacen(tag, &new_tag, 1), old_name))
}

fn tags(page: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        tags.push(&rest[start..start + end + 1]);
        rest = &rest[start + end + 1..];
    }
    tags
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    Some(&tag[start..start + tag[start..].find('"')?])
}

/// The `(url, integrity)` of every script and link in `page` that carries integrity.
fn integrity_references(page: &str) -> Vec<(&str, &str)> {
    tags(page)
        .into_iter()
        .filter(|tag| tag.starts_with("<link") || tag.starts_with("<script"))
        .filter_map(|tag| {
            let url = attribute(tag, "href").or_else(|| attribute(tag, "src"))?;
            Some((url, attribute(tag, "integrity")?))
        })
        .collect()
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    run_status(Command::new("cp").arg("-a").arg(from).arg(to))
}

fn build_full_release(root: &Path, bundle: &Path) -> Result<Value> {
    web::build(root, &CLIENTS, Profile::Release, None)?;
    for client in CLIENTS {
        let target = bundle.join(client.dir);
        fs::create_dir_all(&target)?;
        copy_dir(
            &web::dist_dir(root, client, Profile::Release),
            &target.join("dist"),
        )?;
    }
    Ok(json!({ "built": CLIENTS.map(|client| format!("{}/dist", client.dir)) }))
}

fn command_client(root: &Path, git: &Git, args: &Args) -> Result<()> {
    ensure!(
        git.text(&["status", "--porcelain", "--untracked-files=no"])?
            .is_empty(),
        "commit or stash changes first: a release is built from a commit"
    );
    require_image_tools(root)?;
    let head = git.resolve("HEAD")?;
    let receipts = Receipts::new(root, "client")?;
    let scratch = root.join(".data/releases/scratch");
    if scratch.exists() {
        fs::remove_dir_all(&scratch)?;
    }
    fs::create_dir_all(&scratch)?;
    let base = if args.has("from-target") {
        let target = Target::new(args)?;
        let current = target.status()?["current"]
            .as_str()
            .context("the target has no current release to build on")?
            .to_owned();
        let base = scratch.join("base");
        fs::create_dir_all(&base)?;
        receipts.stage("fetch-base", || {
            let archive = target.run(&["export", &current], None)?;
            let mut tar = Command::new("tar")
                .arg("-x")
                .arg("-C")
                .arg(&base)
                .stdin(Stdio::piped())
                .spawn()?;
            tar.stdin.take().context("tar stdin")?.write_all(&archive)?;
            ensure!(
                tar.wait()?.success(),
                "tar could not extract the base release"
            );
            Ok(json!({ "release": current }))
        })?;
        Some(base)
    } else {
        args.value("base-release").map(PathBuf::from)
    };
    let bundle = args.value("out").map_or_else(
        || root.join(format!(".data/releases/bundle-{}", &head[..12])),
        PathBuf::from,
    );
    if bundle.exists() {
        fs::remove_dir_all(&bundle)?;
    }
    let base_source = match &base {
        Some(base) => {
            serde_json::from_slice::<Value>(&fs::read(base.join("release.json"))?)?["source_commit"]
                .as_str()
                .map(str::to_owned)
        }
        None => None,
    };
    let (reuse, full_build_reason) = match &base {
        Some(_) => css_only(git, base_source.as_deref(), &head),
        None => (false, Some("no base release".into())),
    };
    let detail = match (&base, reuse) {
        (Some(base), true) => {
            receipts.stage("build-css", || build_css_release(root, base, &bundle))?
        }
        _ => receipts.stage("build-clients", || build_full_release(root, &bundle))?,
    };
    let backend_key = git.backend_key(&head)?;
    let manifest = json!({
        "source_commit": head,
        "backend_key": backend_key,
        "files": file_digests(&bundle)?,
    });
    fs::write(
        bundle.join("release.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    let mut result = json!({
        "bundle": bundle,
        "source_commit": head,
        "backend_key": backend_key,
        "kind": if reuse { "css" } else { "full" },
        "base_source": base_source,
        "full_build_reason": full_build_reason,
    });
    if let (Value::Object(result), Value::Object(detail)) = (&mut result, detail) {
        result.extend(detail);
    }
    print_json(&result)
}

/// The server container to release into, and its public URL.
struct Target {
    prefix: Vec<String>,
    url: String,
}

impl Target {
    fn new(args: &Args) -> Result<Self> {
        let prefix = args
            .value("exec")
            .map(str::to_owned)
            .or_else(|| std::env::var("SEQUOIA_RELEASE_EXEC").ok())
            .filter(|prefix| !prefix.is_empty())
            .context("set SEQUOIA_RELEASE_EXEC or pass --exec")?;
        let url = args
            .value("url")
            .map(str::to_owned)
            .or_else(|| std::env::var("SEQUOIA_RELEASE_URL").ok())
            .unwrap_or_default();
        Ok(Self {
            prefix: shell_words(&prefix)?,
            url: url.trim_end_matches('/').to_owned(),
        })
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut command = Command::new(&self.prefix[0]);
        command
            .args(&self.prefix[1..])
            .args([SERVER, "client-release"])
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .context("start the release target command")?;
        let writer = stdin.map(|data| {
            let mut pipe = child.stdin.take().expect("piped stdin");
            let data = data.to_vec();
            std::thread::spawn(move || pipe.write_all(&data))
        });
        let output = child.wait_with_output()?;
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        ensure!(
            output.status.success(),
            "client-release {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output.stdout)
    }

    fn json(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Value> {
        Ok(serde_json::from_slice(&self.run(args, stdin)?)?)
    }

    fn status(&self) -> Result<Value> {
        self.json(&["status"], None)
    }
}

/// Splits a command prefix like a POSIX shell: whitespace, single and double quotes,
/// backslash escapes.
fn shell_words(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => words.extend(word.take()),
            '\'' => {
                let current = word.get_or_insert_default();
                loop {
                    match chars.next().context("unterminated single quote")? {
                        '\'' => break,
                        c => current.push(c),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_default();
                loop {
                    match chars.next().context("unterminated double quote")? {
                        '"' => break,
                        '\\' => current.push(chars.next().context("trailing backslash")?),
                        c => current.push(c),
                    }
                }
            }
            '\\' => word
                .get_or_insert_default()
                .push(chars.next().context("trailing backslash")?),
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    ensure!(!words.is_empty(), "the command prefix is empty");
    Ok(words)
}

fn bundle_tar(bundle: &Path) -> Result<Vec<u8>> {
    run_output(
        Command::new("tar")
            .args([
                "--format=pax",
                "--owner=0",
                "--group=0",
                "--numeric-owner",
                "--mtime=@0",
            ])
            .args(["--sort=name", "-cf", "-", "-C"])
            .arg(bundle)
            .arg("."),
    )
}

/// GETs `url` without content encoding; returns the status and body.
fn fetch(url: &str) -> Result<(u16, Vec<u8>)> {
    let output = run_output(
        Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "30",
                "-H",
                "Accept-Encoding: identity",
                "-A",
                USER_AGENT,
            ])
            .args(["-w", "\n%{http_code}", url]),
    )?;
    let split = output
        .iter()
        .rposition(|&byte| byte == b'\n')
        .context("curl output")?;
    let status = std::str::from_utf8(&output[split + 1..])?.trim().parse()?;
    Ok((status, output[..split].to_vec()))
}

/// The served pages are the activated release's and every integrity-checked asset loads.
fn verify_served(target: &Target, manifest: &Value, asset_version: Option<&str>) -> Result<Value> {
    ensure!(
        !target.url.is_empty(),
        "set SEQUOIA_RELEASE_URL or pass --url to check the served release"
    );
    let mut checked = Vec::new();
    for (client, page, mount) in CLIENT_PAGES {
        let (status, body) = fetch(&format!("{}{page}", target.url))?;
        ensure!(status == 200, "{page} answered {status}");
        let html = String::from_utf8(body)?;
        let references = integrity_references(&html);
        ensure!(
            client != "client/dist" || !references.is_empty(),
            "the map page has no integrity-checked assets"
        );
        for (href, integrity) in references {
            let name = format!("{client}/{}", href.strip_prefix(mount).unwrap_or(href));
            ensure!(
                manifest["files"].get(&name).is_some(),
                "{page} references {href}, which is not in the release"
            );
            let (_, data) = fetch(&format!("{}{href}", target.url))?;
            ensure!(
                web::sri(&data) == integrity,
                "{href} does not match its integrity"
            );
            checked.push(href.to_owned());
        }
        if let Some(version) = asset_version.filter(|_| client == "client/dist") {
            ensure!(
                html.contains(&format!("?v={version}")),
                "the map page does not carry the release's asset version"
            );
        }
    }
    let (status, _) = fetch(&format!("{}/api/health", target.url))?;
    ensure!(status == 200, "/api/health answered {status}");
    Ok(json!(checked))
}

fn command_activate(args: &Args) -> Result<()> {
    let target = Target::new(args)?;
    let bundle = PathBuf::from(args.positional.first().context("activate needs a bundle")?);
    let manifest: Value = serde_json::from_slice(&fs::read(bundle.join("release.json"))?)?;
    let root = crate::repo_root()?;
    let receipts = Receipts::new(&root, "activate")?;
    let before = receipts.stage("status", || target.status())?;
    let expected = before["current"].as_str().unwrap_or("none").to_owned();
    ensure!(
        before["server_backend_key"] == manifest["backend_key"],
        "the bundle was built for backend {}; the target runs {}. Deliver this change with a server release.",
        manifest["backend_key"],
        before["server_backend_key"]
    );
    if args.has("dry-run") {
        return print_json(&json!({
            "would_install": bundle,
            "replacing": before["current"],
            "files": manifest["files"].as_object().map_or(0, |files| files.len()),
        }));
    }
    let archive = bundle_tar(&bundle)?;
    let installed = receipts.stage("install", || target.json(&["install"], Some(&archive)))?;
    let release = installed["id"]
        .as_str()
        .context("install returned no id")?
        .to_owned();
    let activation = receipts.stage("activate", || {
        target.json(&["activate", &release, "--expect", &expected], None)
    })?;
    let status = target.status()?;
    let version = status["releases"]
        .as_array()
        .and_then(|releases| {
            releases
                .iter()
                .find(|entry| entry["id"] == release.as_str())
        })
        .and_then(|entry| entry["asset_version"].as_str())
        .map(str::to_owned);
    let checked = match receipts.stage("verify", || {
        verify_served(&target, &manifest, version.as_deref())
    }) {
        Ok(checked) => checked,
        Err(error) => {
            if activation["changed"] == true {
                receipts.stage("rollback", || {
                    target.json(&["rollback", "--expect", &release], None)
                })?;
            }
            bail!("the served release failed its check and was rolled back: {error:#}");
        }
    };
    print_json(&json!({
        "current": release,
        "previous": activation["previous"],
        "changed": activation["changed"],
        "checked_assets": checked,
        "pruned": activation["pruned"],
    }))
}

fn command_rollback(args: &Args) -> Result<()> {
    let target = Target::new(args)?;
    let receipts = Receipts::new(&crate::repo_root()?, "rollback")?;
    let before = receipts.stage("status", || target.status())?;
    if args.has("dry-run") {
        return print_json(&json!({
            "current": before["current"],
            "would_activate": before["rollback_target"],
        }));
    }
    let current = before["current"].as_str().unwrap_or("none").to_owned();
    let activation = receipts.stage("rollback", || {
        target.json(&["rollback", "--expect", &current], None)
    })?;
    print_json(&activation)
}

/// The deployed Compose project: a `docker compose` prefix that already names its project,
/// files and environment.
struct Compose(Vec<String>);

impl Compose {
    /// Runs a Compose command; its stdout and stderr, kept apart. Compose writes data such as
    /// `--format json` to stdout, and warnings and progress to stderr.
    fn run(&self, args: &[&str]) -> Result<(String, String)> {
        let output = Command::new(&self.0[0])
            .args(&self.0[1..])
            .args(args)
            .output()
            .context("start the Compose command")?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            let tail: String = stderr
                .trim()
                .chars()
                .rev()
                .take(600)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            bail!("compose {}: {tail}", args[0]);
        }
        Ok((String::from_utf8_lossy(&output.stdout).into_owned(), stderr))
    }

    /// Everything a Compose command printed, for receipts and dry-run plans.
    fn output(&self, args: &[&str]) -> Result<Value> {
        let (stdout, stderr) = self.run(args)?;
        Ok(json!(format!("{stdout}{stderr}")))
    }

    fn rows(&self, args: &[&str]) -> Result<Vec<Value>> {
        let args: Vec<&str> = args.iter().copied().chain(["--format", "json"]).collect();
        parse_rows(&self.run(&args)?.0)
    }

    /// Each service's container, image and state, for every service in the project.
    fn identities(&self) -> Result<BTreeMap<String, Value>> {
        let images: BTreeMap<String, String> = self
            .rows(&["images"])?
            .into_iter()
            .filter_map(|row| {
                Some((
                    row["ContainerName"].as_str()?.to_owned(),
                    row["ID"].as_str()?.to_owned(),
                ))
            })
            .collect();
        Ok(self
            .rows(&["ps", "--all"])?
            .into_iter()
            .filter_map(|row| {
                let truncate =
                    |text: &str, length: usize| text.chars().take(length).collect::<String>();
                let image_id = row["Name"]
                    .as_str()
                    .and_then(|name| images.get(name))
                    .map_or("", String::as_str);
                Some((
                    row["Service"].as_str()?.to_owned(),
                    json!({
                        "container": truncate(row["ID"].as_str().unwrap_or(""), 12),
                        "image": row["Image"],
                        "image_id": truncate(image_id, 19),
                        "state": row["State"],
                        "health": row["Health"].as_str().unwrap_or(""),
                    }),
                ))
            })
            .collect())
    }
}

/// Compose's `--format json` output: one array, or one object per line.
fn parse_rows(stdout: &str) -> Result<Vec<Value>> {
    let stdout = stdout.trim();
    if stdout.starts_with('[') {
        return Ok(serde_json::from_str(stdout)?);
    }
    stdout
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

/// Which services were replaced, failing when one outside `selected` was or a selected one is
/// not running and healthy.
fn check_update(
    before: &BTreeMap<String, Value>,
    after: &BTreeMap<String, Value>,
    selected: &[&str],
) -> Result<Vec<String>> {
    let replaced: Vec<String> = after
        .iter()
        .filter(|(service, identity)| {
            before.get(*service).map(|old| &old["container"]) != Some(&identity["container"])
        })
        .map(|(service, _)| service.clone())
        .collect();
    let unrelated: Vec<_> = replaced
        .iter()
        .filter(|service| !selected.contains(&service.as_str()))
        .cloned()
        .collect();
    ensure!(
        unrelated.is_empty(),
        "services outside the selection were replaced: {}",
        unrelated.join(", ")
    );
    let unhealthy: Vec<_> = selected
        .iter()
        .filter(|service| {
            after.get(**service).is_none_or(|identity| {
                identity["state"] != "running"
                    || !matches!(identity["health"].as_str(), Some("" | "healthy"))
            })
        })
        .copied()
        .collect();
    ensure!(
        unhealthy.is_empty(),
        "not running and healthy after the update: {}",
        unhealthy.join(", ")
    );
    Ok(replaced)
}

fn command_services(args: &Args) -> Result<()> {
    let mut services: Vec<&str> = Vec::new();
    for name in &args.positional {
        let service = ALL_SERVICES
            .into_iter()
            .find(|service| service == name)
            .with_context(|| {
                format!("{name} is not a service this command updates: use server, ingest or edge")
            })?;
        if !services.contains(&service) {
            services.push(service);
        }
    }
    ensure!(
        !services.is_empty(),
        "name at least one of server, ingest or edge"
    );
    services.sort_by_key(|service| ALL_SERVICES.iter().position(|known| known == service));
    let prefix = args
        .value("compose")
        .map(str::to_owned)
        .or_else(|| std::env::var("SEQUOIA_COMPOSE_EXEC").ok())
        .filter(|prefix| !prefix.is_empty())
        .context("set SEQUOIA_COMPOSE_EXEC or pass --compose")?;
    let compose = Compose(shell_words(&prefix)?);
    let receipts = Receipts::new(&crate::repo_root()?, "services")?;
    let before = receipts.stage("before", || Ok(json!(compose.identities()?)))?;
    let before: BTreeMap<String, Value> = serde_json::from_value(before)?;
    let missing: Vec<_> = services
        .iter()
        .filter(|service| !before.contains_key(**service))
        .copied()
        .collect();
    ensure!(
        missing.is_empty(),
        "the project has no {} service",
        missing.join(", ")
    );
    let up: Vec<&str> = ["up", "--detach", "--no-deps", "--wait", "--pull", "never"]
        .into_iter()
        .chain(services.iter().copied())
        .collect();
    let pull: Vec<&str> = ["pull"]
        .into_iter()
        .chain(services.iter().copied())
        .collect();
    if args.has("dry-run") {
        let plan = receipts.stage("dry-run", || {
            let mut pull_plan = vec!["--dry-run"];
            pull_plan.extend(&pull);
            let mut up_plan = vec!["--dry-run"];
            up_plan.extend(&up);
            Ok(json!(format!(
                "{}{}",
                compose.output(&pull_plan)?.as_str().unwrap_or_default(),
                compose.output(&up_plan)?.as_str().unwrap_or_default()
            )))
        })?;
        return print_json(&json!({
            "dry_run": true,
            "services": services,
            "compose": plan.as_str().unwrap_or_default().trim().lines().collect::<Vec<_>>(),
            "before": before,
        }));
    }
    receipts.stage("pull", || compose.output(&pull))?;
    receipts.stage("up", || compose.output(&up))?;
    let after: BTreeMap<String, Value> =
        serde_json::from_value(receipts.stage("after", || Ok(json!(compose.identities()?)))?)?;
    let replaced = check_update(&before, &after, &services)?;
    print_json(&json!({
        "services": services,
        "unchanged": after.keys().filter(|service| !replaced.contains(service)).collect::<Vec<_>>(),
        "replaced": replaced,
        "before": before,
        "after": after,
    }))
}

/// Timed stage receipts, appended as JSON lines and echoed to stderr.
struct Receipts {
    action: &'static str,
    path: PathBuf,
}

impl Receipts {
    fn new(root: &Path, action: &'static str) -> Result<Self> {
        let dir = root.join(".data/releases");
        fs::create_dir_all(&dir)?;
        Ok(Self {
            action,
            path: dir.join("receipts.jsonl"),
        })
    }

    fn stage(&self, name: &str, work: impl FnOnce() -> Result<Value>) -> Result<Value> {
        let started_unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let clock = Instant::now();
        let result = work();
        let (outcome, detail) = match &result {
            Ok(detail) => ("ok", detail.clone()),
            Err(error) => ("failed", json!(format!("{error:#}"))),
        };
        let record = json!({
            "action": self.action,
            "stage": name,
            "outcome": outcome,
            "started_unix": started_unix,
            "seconds": (clock.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
            "detail": detail,
        });
        eprintln!("{record}");
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
            "{record}"
        )?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_css_release_points_the_one_stylesheet_link_at_the_new_file() {
        let page = "<head><link rel=\"modulepreload\" href=\"/app-1.js\" integrity=\"sha384-js\">\
                    <link rel=\"stylesheet\" href=\"/input-old.css\" integrity=\"sha384-old\"/></head>";
        let (html, old) = swap_stylesheet(page, "input-new.css", "sha384-new").expect("swap");
        assert_eq!(old, "input-old.css");
        assert!(html.contains(
            "<link rel=\"stylesheet\" href=\"/input-new.css\" integrity=\"sha384-new\"/>"
        ));
        assert!(html.contains("href=\"/app-1.js\" integrity=\"sha384-js\""));
        assert_eq!(
            integrity_references(&html),
            [("/app-1.js", "sha384-js"), ("/input-new.css", "sha384-new")]
        );
    }

    #[test]
    fn compose_rows_come_from_stdout_in_either_json_shape() {
        assert_eq!(
            parse_rows("[{\"Service\":\"server\"}]\n")
                .expect("array")
                .len(),
            1
        );
        let lines =
            parse_rows("{\"Service\":\"server\"}\n{\"Service\":\"edge\"}\n").expect("lines");
        assert_eq!(lines[1]["Service"], "edge");
    }

    #[test]
    fn a_selective_update_fails_when_anything_else_was_replaced_or_is_unhealthy() {
        let identity = |container: &str, health: &str| json!({ "container": container, "state": "running", "health": health });
        let before = BTreeMap::from([
            ("server".to_owned(), identity("a", "healthy")),
            ("postgres".to_owned(), identity("p", "healthy")),
        ]);
        let updated = BTreeMap::from([
            ("server".to_owned(), identity("b", "healthy")),
            ("postgres".to_owned(), identity("p", "healthy")),
        ]);
        assert_eq!(
            check_update(&before, &updated, &["server"]).expect("ok"),
            ["server"]
        );

        let unrelated = BTreeMap::from([
            ("server".to_owned(), identity("b", "healthy")),
            ("postgres".to_owned(), identity("q", "healthy")),
        ]);
        assert!(check_update(&before, &unrelated, &["server"]).is_err());

        let unhealthy = BTreeMap::from([
            ("server".to_owned(), identity("b", "unhealthy")),
            ("postgres".to_owned(), identity("p", "healthy")),
        ]);
        assert!(check_update(&before, &unhealthy, &["server"]).is_err());
    }

    #[test]
    fn command_prefixes_split_like_a_shell() {
        assert_eq!(
            shell_words("ssh host 'docker exec -i' \"a b\" c\\ d").expect("split"),
            ["ssh", "host", "docker exec -i", "a b", "c d"]
        );
    }
}
