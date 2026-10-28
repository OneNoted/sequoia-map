//! The development watcher: rebuilds the smallest affected part after each save and tells
//! open pages what changed.
//!
//! It runs as the `watch` Mise daemon beside `postgres`, `api` and `ingest`, which Mise's
//! supervisor starts, checks and stops. A server or gateway change is built first and the
//! daemon restarted only when the build succeeds, so a compile error leaves the last good
//! process running. Pages connect to `/events` on `WATCH_PORT`.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};

use crate::web::{self, CLAIMS, Client, MAP, Page, Profile};
use crate::{run_diagnosed, run_status};

/// Directories whose contents feed a build; `dist` and `target` below them are ignored.
const WATCHED_DIRS: [&str; 7] = [
    "client",
    "claims-client",
    "browser-map",
    "wasm",
    "shared",
    "server",
    "services/sequoia-ingest/src",
];
const WATCHED_FILES: [&str; 4] = [
    "Cargo.toml",
    "Cargo.lock",
    "services/sequoia-ingest/Cargo.toml",
    "services/sequoia-ingest/Cargo.lock",
];

/// What a set of changed paths needs rebuilt.
#[derive(Debug, Default, PartialEq, Eq)]
struct Work {
    wasm: BTreeSet<&'static str>,
    css: BTreeSet<&'static str>,
    pages: BTreeSet<&'static str>,
    assets: bool,
    server: bool,
    ingest: bool,
}

impl Work {
    fn is_empty(&self) -> bool {
        *self == Work::default()
    }

    fn add(&mut self, path: &str) {
        let both = [MAP.name, CLAIMS.name];
        match path {
            "Cargo.toml" | "Cargo.lock" => {
                self.wasm.extend(both);
                self.server = true;
                self.ingest = true;
            }
            "client/input.css" => {
                self.css.insert(MAP.name);
            }
            "claims-client/input.css" => {
                self.css.insert(CLAIMS.name);
            }
            // Tailwind reads the map page and its Rust sources for class names.
            "client/index.html" => {
                self.pages.insert(MAP.name);
                self.css.insert(MAP.name);
            }
            "claims-client/index.html" => {
                self.pages.insert(CLAIMS.name);
            }
            _ if path.starts_with("client/public/") => self.assets = true,
            _ if path.starts_with("client/src/") || path == "client/Cargo.toml" => {
                self.wasm.insert(MAP.name);
                self.css.insert(MAP.name);
            }
            _ if path.starts_with("claims-client/src/") || path == "claims-client/Cargo.toml" => {
                self.wasm.insert(CLAIMS.name);
            }
            _ if path.starts_with("browser-map/") || path.starts_with("wasm/") => {
                self.wasm.extend(both);
            }
            _ if path.starts_with("shared/") => {
                self.wasm.extend(both);
                self.server = true;
                self.ingest = true;
            }
            _ if path.starts_with("server/") => self.server = true,
            _ if path.starts_with("services/sequoia-ingest/") => self.ingest = true,
            _ => {}
        }
    }
}

fn client(name: &str) -> &'static Client {
    if name == MAP.name { &MAP } else { &CLAIMS }
}

/// `xtask watch`
pub fn command(root: &Path, _args: &[String]) -> Result<()> {
    let port: u16 = std::env::var("WATCH_PORT")
        .context("WATCH_PORT is not set; start the watcher with `mise run dev`")?
        .parse()
        .context("WATCH_PORT is not a port")?;
    let listeners = Listeners::serve(port)?;

    let (sender, changes) = channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(50), None, sender)?;
    for dir in WATCHED_DIRS {
        debouncer.watch(root.join(dir), RecursiveMode::Recursive)?;
    }
    for file in WATCHED_FILES {
        debouncer.watch(root.join(file), RecursiveMode::NonRecursive)?;
    }

    // The pages `mise run dev` built, rebuilt here with this watcher's reload script.
    let started = Instant::now();
    let built = web::build(root, &[&MAP, &CLAIMS], Profile::Dev, Some(port))?;
    let mut pages: [Page; 2] = [built[0].clone(), built[1].clone()];
    eprintln!(
        "watch: pages ready in {:.1}s; reload events on http://127.0.0.1:{port}/events",
        started.elapsed().as_secs_f64()
    );

    loop {
        let work = next_work(root, &changes)?;
        let started = Instant::now();
        match rebuild(root, &work, &mut pages, port, &listeners) {
            Ok(()) => eprintln!("watch: done in {:.2}s", started.elapsed().as_secs_f64()),
            Err(error) => {
                let message = format!("{error:#}");
                eprintln!("watch: build failed: {message}");
                listeners.send("failed", &message);
            }
        }
    }
}

/// Waits for changes, then collects everything else saved within the debounce window.
fn next_work(root: &Path, changes: &Receiver<DebounceEventResult>) -> Result<Work> {
    let mut work = Work::default();
    loop {
        let batch = if work.is_empty() {
            changes.recv().context("the file watcher stopped")?
        } else {
            match changes.recv_timeout(Duration::from_millis(30)) {
                Ok(batch) => batch,
                Err(RecvTimeoutError::Timeout) => return Ok(work),
                Err(RecvTimeoutError::Disconnected) => anyhow::bail!("the file watcher stopped"),
            }
        };
        let Ok(events) = batch else { continue };
        for event in events.iter().filter(|event| !event.kind.is_access()) {
            for path in &event.paths {
                if let Some(relative) = relevant(root, path) {
                    eprintln!("watch: {relative} ({:?})", event.kind);
                    work.add(&relative);
                }
            }
        }
    }
}

fn relevant(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?.to_str()?.to_owned();
    let generated = relative
        .split('/')
        .any(|part| matches!(part, "dist" | "target") || part.starts_with('.'));
    let scratch = relative.ends_with('~') || relative.ends_with(".swp");
    (!generated && !scratch).then_some(relative)
}

fn rebuild(
    root: &Path,
    work: &Work,
    pages: &mut [Page; 2],
    port: u16,
    listeners: &Listeners,
) -> Result<()> {
    let index = |name: &str| usize::from(name == CLAIMS.name);
    let mut reload = BTreeSet::new();

    // The stylesheet first: it is the fastest feedback and needs no reload.
    for &name in &work.css {
        pages[index(name)].css = web::stylesheet(root, client(name), Profile::Dev)?;
    }
    if work.assets {
        for client in [&MAP, &CLAIMS] {
            web::link_public(root, client, Profile::Dev)?;
            reload.insert(client.name);
        }
    }
    reload.extend(work.pages.iter().copied());
    for name in [MAP.name, CLAIMS.name] {
        if work.css.contains(name) || reload.contains(name) {
            web::write_page(
                root,
                client(name),
                Profile::Dev,
                &pages[index(name)],
                Some(port),
            )?;
        }
    }
    for &name in &work.css {
        if !reload.contains(name) {
            let href = format!("{}{}", mount(name), pages[index(name)].css);
            listeners.send(
                "css",
                &serde_json::json!({ "client": name, "href": href }).to_string(),
            );
        }
    }

    if !work.wasm.is_empty() {
        let clients: Vec<_> = work.wasm.iter().map(|&name| client(name)).collect();
        web::compile(root, &clients, Profile::Dev)?;
        for client in clients {
            let (js, wasm) = web::bindgen(root, client, Profile::Dev)?;
            let page = &mut pages[index(client.name)];
            page.js = js;
            page.wasm = wasm;
            web::write_page(root, client, Profile::Dev, page, Some(port))?;
            reload.insert(client.name);
        }
    }
    for &name in &reload {
        listeners.send("reload", name);
    }

    if work.server {
        run_diagnosed(Command::new(cargo()).current_dir(root).args([
            "build",
            "--locked",
            "--color",
            "never",
            "-p",
            "sequoia-server",
        ]))?;
        restart_daemon("api")?;
        listeners.send("reload", "all");
    }
    if work.ingest && daemon_running("ingest")? {
        run_diagnosed(Command::new(cargo()).current_dir(root).args([
            "build",
            "--locked",
            "--color",
            "never",
            "--manifest-path",
            "services/sequoia-ingest/Cargo.toml",
        ]))?;
        restart_daemon("ingest")?;
    }
    Ok(())
}

fn mount(name: &str) -> &'static str {
    if name == CLAIMS.name {
        "/claims-app/"
    } else {
        "/"
    }
}

fn cargo() -> PathBuf {
    std::env::var_os("CARGO").map_or_else(|| "cargo".into(), PathBuf::from)
}

/// Replaces one Mise daemon and waits for its readiness check. `restart` would also restart
/// its dependencies (PostgreSQL for the API); `start` leaves running dependencies alone.
fn restart_daemon(name: &str) -> Result<()> {
    run_status(Command::new("mise").args(["daemons", "stop", name]))?;
    run_status(Command::new("mise").args(["daemons", "start", name]))
}

fn daemon_running(name: &str) -> Result<bool> {
    let output = Command::new("mise")
        .args(["daemons", "ls", "--json"])
        .output()
        .context("list daemons")?;
    let daemons: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    Ok(daemons.as_array().is_some_and(|daemons| {
        daemons
            .iter()
            .any(|daemon| daemon["name"] == name && daemon["status"] == "running")
    }))
}

/// Open pages listening for server-sent events.
#[derive(Clone)]
struct Listeners(Arc<Mutex<Vec<TcpStream>>>);

impl Listeners {
    fn serve(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .with_context(|| format!("bind the reload port {port}"))?;
        let listeners = Self(Arc::default());
        let accepted = listeners.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let accepted = accepted.clone();
                std::thread::spawn(move || accepted.accept(stream));
            }
        });
        Ok(listeners)
    }

    fn accept(&self, mut stream: TcpStream) {
        // A page that stops reading must not stall the watcher: a write that cannot finish
        // within the timeout drops that page, which reconnects on its own.
        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
                    Access-Control-Allow-Origin: *\r\n\r\nretry: 1000\n\n";
        if stream.write_all(head.as_bytes()).is_ok() {
            self.0.lock().expect("listeners lock").push(stream);
        }
    }

    fn send(&self, event: &str, data: &str) {
        let mut message = format!("event: {event}\n");
        for line in data.lines() {
            message.push_str(&format!("data: {line}\n"));
        }
        message.push('\n');
        self.0
            .lock()
            .expect("listeners lock")
            .retain_mut(|stream| stream.write_all(message.as_bytes()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(paths: &[&str]) -> Work {
        let mut work = Work::default();
        for path in paths {
            work.add(path);
        }
        work
    }

    #[test]
    fn each_edit_rebuilds_only_what_reads_it() {
        let stylesheet = work(&["client/input.css"]);
        assert_eq!(stylesheet.css, BTreeSet::from(["map"]));
        assert!(stylesheet.wasm.is_empty() && !stylesheet.server);

        let map = work(&["client/src/app.rs"]);
        assert_eq!(map.wasm, BTreeSet::from(["map"]));
        assert_eq!(map.css, BTreeSet::from(["map"]));

        let claims = work(&["claims-client/src/main.rs"]);
        assert_eq!(claims.wasm, BTreeSet::from(["claims"]));
        assert!(claims.css.is_empty());

        let server = work(&["server/src/routes/api.rs"]);
        assert!(server.server && server.wasm.is_empty() && !server.ingest);

        let shared = work(&["shared/src/lib.rs"]);
        assert_eq!(shared.wasm, BTreeSet::from(["claims", "map"]));
        assert!(shared.server && shared.ingest);
    }

    #[test]
    fn generated_and_editor_files_are_ignored() {
        let root = Path::new("/repo");
        assert_eq!(
            relevant(root, Path::new("/repo/client/dist/index.html")),
            None
        );
        assert_eq!(
            relevant(root, Path::new("/repo/client/src/.app.rs.swp")),
            None
        );
        assert_eq!(
            relevant(root, Path::new("/repo/client/src/app.rs")).as_deref(),
            Some("client/src/app.rs")
        );
    }
}
