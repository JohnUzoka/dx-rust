//! Over-the-air updates for DX RUST's Nintendo Switch build (the game shows them: crates/dxview/src/horizon/update.rs).
//!
//! A check reads the fork's GitHub releases and offers the newest one tagged `switch-v<version>` with a version
//! newer than the running NRO's and no pre-release part, whose `dxview.nro` GitHub lists a SHA-256 for
//! ([`release::newer`]). The game reads the player's own Deus Ex files, which no update changes, so any newer
//! release fits them.
//!
//! A download streams the release's NRO to `<nro>.update.tmp` beside the running one and checks its size, the
//! SHA-256 GitHub lists for it, and that it is a whole NRO whose NACP shows the release's version. Then the running
//! NRO moves to `<nro>.update.old` and the download takes its place. That is safe while the game runs: the homebrew
//! loader read the whole NRO into memory before starting it, and the game has no RomFS, so nothing reads the file
//! again. The next start runs the new version, and [`recover`] there deletes the old one, or puts it back if an
//! update was cut short.
//!
//! The same machinery as Skate 3 Rust Engine's Switch updates (JohnUzoka/skate-3-rust-engine,
//! crates/skate-switch-update), without its check that a release reads the SD card's exported game files.
#[cfg(any(target_os = "horizon", feature = "system-curl"))]
mod curl;
mod install;
mod nro;
mod release;

#[cfg(any(target_os = "horizon", feature = "system-curl"))]
pub use curl::Curl;
pub use install::recover;
pub use semver::Version;

use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
    thread::JoinHandle,
};

/// The repository whose releases the Switch build updates from.
pub const REPOSITORY: &str = "JohnUzoka/dx-rust-nx";
/// The repository's earlier names. GitHub sends a renamed repository's API calls on to it, but its listing then
/// names the new repository in every download, which a build checking for its old name rejects: builds before
/// 2026-11-06's rename find no updates. Downloads under these are accepted too, so a rename back keeps updates working.
pub const FORMER_NAMES: &[&str] = &["JohnUzoka/dx-rust"];
/// Switch releases are tagged `switch-v<version>`, the version their NRO's NACP shows (switch/build.sh).
pub const TAG_PREFIX: &str = "switch-v";
/// The release file with the NRO.
pub const NRO_ASSET: &str = "dxview.nro";

/// The newest releases a check reads.
const RELEASES_PER_PAGE: u32 = 20;
const LISTING_LIMIT: u64 = 4 << 20;
/// The NRO goes to the SD card in writes this size.
const WRITE_BUFFER: usize = 1 << 20;

/// Why a check or a download stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

/// How the updater reaches GitHub: [`Curl`], or fixed answers in tests.
pub trait Transport: Send + Sync + 'static {
    /// Writes the body of an HTTPS GET of `url`, following redirects, to `sink`. Fails on an HTTP error, a body
    /// longer than `limit` or a failed write, and returns [`Error::Cancelled`] soon after `cancel` is set.
    fn get(&self, url: &str, accept: &str, limit: u64, sink: &mut dyn Write, cancel: &AtomicBool) -> Result<(), Error>;
}

/// What the updater compares releases with, and where the running NRO is.
#[derive(Clone, Debug)]
pub struct Settings {
    pub repository: String,
    /// The running NRO's version.
    pub installed: Version,
    /// The running NRO's path, `argv[0]`.
    pub nro: PathBuf,
}

impl Settings {
    /// This game's releases, for the NRO at `nro` built as version `installed`.
    pub fn new(installed: &str, nro: impl Into<PathBuf>) -> Result<Self, String> {
        Ok(Self {
            repository: REPOSITORY.into(),
            installed: Version::parse(installed.trim()).map_err(|e| format!("{installed:?} isn't a version: {e}"))?,
            nro: nro.into(),
        })
    }
}

/// A release the updater can install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub tag: String,
    /// The release notes, as written on GitHub.
    pub notes: String,
    /// The NRO's size in bytes.
    pub size: u64,
    nro: release::Asset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing checked yet.
    Idle,
    Checking,
    /// No newer release.
    UpToDate,
    Available { offer: Offer },
    /// `done` bytes of the NRO so far.
    Downloading { offer: Offer, done: u64 },
    /// The new NRO is in place; the next start runs it.
    Installed { offer: Offer },
    Failed(String),
    Cancelled,
}

struct Shared {
    state: Mutex<State>,
    cancel: AtomicBool,
    done: AtomicU64,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs checks and downloads on a thread of their own. Dropping it cancels what is running and waits for the
/// thread, which leaves nothing open.
pub struct Updater {
    settings: Arc<Settings>,
    transport: Arc<dyn Transport>,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Updater {
    pub fn new(settings: Settings, transport: impl Transport) -> Self {
        Self {
            settings: Arc::new(settings),
            transport: Arc::new(transport),
            shared: Arc::new(Shared { state: Mutex::new(State::Idle), cancel: AtomicBool::new(false), done: AtomicU64::new(0) }),
            worker: None,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn state(&self) -> State {
        match lock(&self.shared.state).clone() {
            State::Downloading { offer, .. } => State::Downloading { offer, done: self.shared.done.load(Ordering::Relaxed) },
            state => state,
        }
    }

    /// Starts a check, unless one or a download is running or an update is installed. False if it didn't start.
    pub fn check(&mut self) -> bool {
        self.start(State::Checking, |settings, transport, shared| match check(settings, transport, &shared.cancel) {
            Ok(Some(offer)) => State::Available { offer },
            Ok(None) => State::UpToDate,
            Err(Error::Cancelled) => State::Cancelled,
            Err(Error::Failed(message)) => State::Failed(message),
        })
    }

    /// Downloads, checks and installs the release a check offered. False if there was none, or it didn't start.
    pub fn download(&mut self) -> bool {
        let State::Available { offer } = self.state() else { return false };
        self.start(State::Downloading { offer: offer.clone(), done: 0 }, move |settings, transport, shared| {
            let installed = download(settings, transport, &offer, shared)
                .and_then(|staged| install::install(&settings.nro, &staged, &offer.version).map_err(Error::Failed));
            match installed {
                Ok(()) => State::Installed { offer },
                Err(Error::Cancelled) => State::Cancelled,
                Err(Error::Failed(message)) => State::Failed(message),
            }
        })
    }

    /// Asks a running check or download to stop.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    /// Joins the thread of a check or download that has finished, which gives its stack back (2 MiB on the
    /// Switch). Call it now and then; dropping the updater joins it too.
    pub fn reap(&mut self) {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.join();
        }
    }

    fn start(&mut self, running: State, job: impl FnOnce(&Settings, &dyn Transport, &Shared) -> State + Send + 'static) -> bool {
        {
            let mut state = lock(&self.shared.state);
            if matches!(*state, State::Checking | State::Downloading { .. } | State::Installed { .. }) {
                return false;
            }
            *state = running;
        }
        // The last job set its state as its last step, so its thread is ending.
        self.join();
        self.shared.cancel.store(false, Ordering::Relaxed);
        self.shared.done.store(0, Ordering::Relaxed);
        let (settings, transport, shared) = (self.settings.clone(), self.transport.clone(), self.shared.clone());
        let spawned = std::thread::Builder::new().name("dx-update".into()).spawn(move || {
            let end = job(&settings, &*transport, &shared);
            *lock(&shared.state) = end;
        });
        match spawned {
            Ok(worker) => {
                self.worker = Some(worker);
                true
            }
            Err(e) => {
                *lock(&self.shared.state) = State::Failed(format!("couldn't start the update thread: {e}"));
                false
            }
        }
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.cancel();
        self.join();
    }
}

/// The newest release to offer, if any.
fn check(settings: &Settings, transport: &dyn Transport, cancel: &AtomicBool) -> Result<Option<Offer>, Error> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page={RELEASES_PER_PAGE}", settings.repository);
    let mut listing = Vec::new();
    transport.get(&url, "application/vnd.github+json", LISTING_LIMIT, &mut listing, cancel)?;
    let candidates = release::newer(&listing, &settings.repository, &settings.installed).map_err(Error::Failed)?;
    Ok(candidates.into_iter().next().map(|candidate| Offer {
        size: candidate.nro.size,
        version: candidate.version,
        tag: candidate.tag,
        notes: candidate.notes,
        nro: candidate.nro,
    }))
}

/// Writes the NRO to the SD card, hashing it on the way.
struct Sink<'a> {
    file: BufWriter<File>,
    hash: Sha256,
    written: u64,
    done: &'a AtomicU64,
}

impl Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write_all(bytes)?;
        self.hash.update(bytes);
        self.written += bytes.len() as u64;
        self.done.store(self.written, Ordering::Relaxed);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Downloads `offer`'s NRO beside the running one and checks it; its path, or nothing left behind.
fn download(settings: &Settings, transport: &dyn Transport, offer: &Offer, shared: &Shared) -> Result<PathBuf, Error> {
    let staged = install::beside(&settings.nro, install::STAGED);
    let result = fetch(transport, offer, &staged, shared);
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result.map(|()| staged)
}

fn fetch(transport: &dyn Transport, offer: &Offer, staged: &Path, shared: &Shared) -> Result<(), Error> {
    let failed = |what: &str, e: std::io::Error| Error::Failed(format!("couldn't {what} {}: {e}", staged.display()));
    let file = File::create(staged).map_err(|e| failed("create", e))?;
    let mut sink = Sink { file: BufWriter::with_capacity(WRITE_BUFFER, file), hash: Sha256::new(), written: 0, done: &shared.done };
    transport.get(&offer.nro.url, "application/octet-stream", offer.nro.size, &mut sink, &shared.cancel)?;
    let Sink { file, hash, written, .. } = sink;
    let file = file.into_inner().map_err(|e| failed("write", e.into_error()))?;
    file.sync_all().map_err(|e| failed("write", e))?;
    drop(file);
    if written != offer.nro.size {
        return Err(Error::Failed(format!("the download is {written} bytes; the release lists {}", offer.nro.size)));
    }
    if hash.finalize().as_slice() != offer.nro.sha256 {
        return Err(Error::Failed("the download doesn't match the SHA-256 GitHub lists for it".into()));
    }
    match nro::version(staged) {
        Ok(text) if Version::parse(&text).is_ok_and(|found| found == offer.version) => Ok(()),
        Ok(text) => Err(Error::Failed(format!("the downloaded NRO shows version {text}, not {}", offer.version))),
        Err(e) => Err(Error::Failed(format!("the download isn't a whole NRO: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    /// Answers GETs from a table, as GitHub would.
    struct Fixed(HashMap<String, Vec<u8>>);

    impl Transport for Fixed {
        fn get(&self, url: &str, _: &str, limit: u64, sink: &mut dyn Write, _: &AtomicBool) -> Result<(), Error> {
            let body = self.0.get(url).ok_or_else(|| Error::Failed(format!("GitHub has no such file: {url}")))?;
            if body.len() as u64 > limit {
                return Err(Error::Failed("too long".into()));
            }
            sink.write_all(body).map_err(|e| Error::Failed(e.to_string()))
        }
    }

    const LISTING: &str = "https://api.github.com/repos/JohnUzoka/dx-rust-nx/releases?per_page=20";

    fn url(tag: &str) -> String {
        format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{NRO_ASSET}")
    }

    /// A GitHub whose releases are `(version, NRO bytes, the digest it lists)`.
    fn github(releases: &[(&str, &[u8], [u8; 32])]) -> Fixed {
        let mut files = HashMap::new();
        let mut listing = Vec::new();
        for (version, nro, digest) in releases {
            let tag = format!("{TAG_PREFIX}{version}");
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            listing.push(json!({ "tag_name": tag, "body": format!("What's new in {version}"), "draft": false, "prerelease": true, "assets": [
                { "name": NRO_ASSET, "size": nro.len(), "state": "uploaded", "digest": format!("sha256:{hex}"), "browser_download_url": url(&tag) },
            ]}));
            files.insert(url(&tag), nro.to_vec());
        }
        files.insert(LISTING.into(), json!(listing).to_string().into_bytes());
        Fixed(files)
    }

    /// An NRO of at least 1 MiB, the smallest a release may list.
    fn nro(version: &str) -> Vec<u8> {
        let mut bytes = nro::fake(version);
        bytes.resize(1 << 20, 0);
        bytes
    }

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn finish(updater: &mut Updater) -> State {
        updater.join();
        updater.state()
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("switch-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn check_offers_the_newest_release_and_nothing_to_the_newest() {
        let (a, b) = (nro("0.1.0"), nro("0.1.1"));
        let releases = || github(&[("0.1.0", &a, sha256(&a)), ("0.1.1", &b, sha256(&b))]);
        let mut updater = Updater::new(Settings::new("0.1.0-otatest", "unused.nro").unwrap(), releases());
        assert!(updater.check());
        let State::Available { offer } = finish(&mut updater) else { panic!("{:?}", updater.state()) };
        assert_eq!((offer.version, offer.notes.as_str()), (Version::new(0, 1, 1), "What's new in 0.1.1"));

        let mut updater = Updater::new(Settings::new("0.1.1", "unused.nro").unwrap(), releases());
        updater.check();
        assert_eq!(finish(&mut updater), State::UpToDate);
    }

    #[test]
    fn download_installs_a_checked_nro_and_leaves_a_bad_one_out() {
        let dir = folder("download");
        let running = dir.join(NRO_ASSET);
        let new = nro("0.1.0");

        // GitHub lists a digest that isn't the file's: nothing changes.
        std::fs::write(&running, nro("0.1.0-otatest")).unwrap();
        let mut updater = Updater::new(Settings::new("0.1.0-otatest", &running).unwrap(), github(&[("0.1.0", &new, [7; 32])]));
        updater.check();
        assert!(matches!(finish(&mut updater), State::Available { .. }));
        assert!(updater.download());
        let State::Failed(message) = finish(&mut updater) else { panic!("{:?}", updater.state()) };
        assert!(message.contains("SHA-256"), "{message}");
        assert_eq!(std::fs::read(&running).unwrap(), nro("0.1.0-otatest"));
        assert!(!dir.join("dxview.nro.update.tmp").exists());

        // The right digest: the new NRO takes the running one's place, which is kept beside it.
        let mut updater = Updater::new(Settings::new("0.1.0-otatest", &running).unwrap(), github(&[("0.1.0", &new, sha256(&new))]));
        updater.check();
        finish(&mut updater);
        updater.download();
        assert!(matches!(finish(&mut updater), State::Installed { .. }), "{:?}", updater.state());
        assert_eq!(std::fs::read(&running).unwrap(), new);
        assert_eq!(std::fs::read(dir.join("dxview.nro.update.old")).unwrap(), nro("0.1.0-otatest"));
        assert!(!updater.check(), "a second check after installing");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
