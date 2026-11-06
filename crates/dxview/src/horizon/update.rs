//! Over-the-air updates: the game's end of crates/switch-update. `init` tidies up after an update at start-up
//! (`recover`). A few seconds after the first frame a check asks GitHub for a newer release; once one is found and a
//! menu is on screen (the title's, or the one Plus opens), a yes/no box offers it. The download runs on a thread of
//! its own while the game goes on, counted by a line at the bottom of the screen; then a box says it is installed
//! for the next start and offers to quit now. A download that fails says why and offers another go; a check that
//! fails (no internet) only goes to log.txt, as every step does (`[Update] ...`).
//!
//! Only builds with a release version check (switch/build.sh's `DXVIEW_VERSION`: a build at a `switch-v*` tag, or
//! one built with `VERSION=`, such as `0.1.0-otatest` for trying an update): a development build never replaces
//! itself. `no_update_check` in options.txt turns it off.
use bevy::prelude::*;
use std::path::PathBuf;
use switch_update::{Curl, State, Updater};

/// The release this NRO is (its NACP's version), or empty for a development build: switch/build.sh.
const VERSION: Option<&str> = option_env!("DXVIEW_VERSION");
/// Seconds after the first frame before the check starts: the title is up and nothing waits on it.
const CHECK_AFTER: f32 = 5.0;
/// How often the update's state is read.
const POLL_EVERY: f32 = 0.25;
/// How long the bottom line goes on saying an update is installed once the box has told the player.
const RESULT_SHOWN: f32 = 10.0;

/// The running NRO, `argv[0]` as libnx's `argvSetup` parsed it (bevy_horizon's `[Boot] launched from` line reads the
/// same), with the SD card's device name if the loader left it out.
fn nro_path() -> Option<PathBuf> {
    extern "C" {
        static __system_argc: std::ffi::c_int;
        static __system_argv: *const *const std::ffi::c_char;
    }
    // SAFETY: libnx sets both before main and nothing changes them after.
    let path = unsafe {
        if __system_argc < 1 || __system_argv.is_null() || (*__system_argv).is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(*__system_argv).to_str().ok()?
    };
    let path = if path.starts_with('/') { format!("sdmc:{path}") } else { path.to_owned() };
    path.to_ascii_lowercase().ends_with(".nro").then(|| PathBuf::from(path))
}

/// At start-up: deletes what an update cut short left beside the NRO, or puts the old NRO back if the new one
/// isn't whole (switch_update::recover).
pub(super) fn recover() {
    let Some(nro) = nro_path() else { return };
    match switch_update::recover(&nro) {
        Ok(None) => {}
        Ok(Some(done)) => eprintln!("[Update] {done}"),
        Err(error) => eprintln!("[Update] couldn't tidy up after the last update: {error}"),
    }
}

/// The updater for this build, or why updates are off (both said in log.txt).
pub(super) fn updater() -> Result<Updater, String> {
    let version = VERSION.filter(|v| !v.is_empty()).ok_or("a development build (no release version)")?;
    let nro = nro_path().ok_or("the game wasn't started from an .nro file")?;
    let settings = switch_update::Settings::new(version, nro)?;
    Ok(Updater::new(settings, Curl::new(&format!("dx-rust-switch/{version}"))))
}

/// The question a yes/no box on screen asks, so its answer goes to the right step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Download,
    Retry,
    Quit,
}

#[derive(Resource)]
pub(super) struct Update {
    updater: Updater,
    asked: Option<Asked>,
    /// The player turned the offer down: not asked again this run.
    declined: bool,
    /// A download was started (a failure after it is the player's business; a failed check isn't).
    downloading: bool,
    /// The player asked for another go after a failure: download as soon as the check offers it again.
    retry: bool,
    /// The box that says the update is installed has been shown.
    told: bool,
    since: f32,
    polled: f32,
    /// Seconds the bottom line still says the update is installed.
    result_left: f32,
    logged: String,
}

impl Update {
    pub(super) fn new(updater: Updater) -> Self {
        Self {
            updater,
            asked: None,
            declined: false,
            downloading: false,
            retry: false,
            told: false,
            since: 0.0,
            polled: 0.0,
            result_left: 0.0,
            logged: String::new(),
        }
    }

    /// A yes/no box on top of whatever menu is up (menus.rs `open_yes_no`); its answer arrives in `update_system`.
    fn ask(&mut self, asked: Asked, title: &str, text: &str) {
        let _ = crate::menus::take_box_answer();
        crate::menus::open_yes_no(title, text);
        self.asked = Some(asked);
    }
}

/// The line at the bottom of the screen while an update downloads.
#[derive(Component)]
pub(super) struct UpdateText;

pub(super) fn spawn_text(mut commands: Commands) {
    commands.spawn((
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(16.0), ..default() },
        TextColor(Color::srgb(1.0, 0.95, 0.55)),
        Node { position_type: PositionType::Absolute, left: Val::Px(10.0), bottom: Val::Px(6.0), ..default() },
        GlobalZIndex(i32::MAX),
        Visibility::Hidden,
        UpdateText,
        crate::loader::Persist,
    ));
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

pub(super) fn update_system(
    time: Res<Time<Real>>,
    mut update: ResMut<Update>,
    line: Single<(&mut Text, &mut Visibility), With<UpdateText>>,
    mut exit: MessageWriter<AppExit>,
) {
    let dt = time.delta_secs();
    let u = &mut *update;
    u.since += dt;
    u.polled += dt;
    u.result_left = (u.result_left - dt).max(0.0);
    let (mut text, mut visibility) = line.into_inner();

    // An answer to the box on screen: Yes, or No (B and Escape answer No). Start closes every menu at once, the box
    // with them, without one: that counts as No.
    if let Some(asked) = u.asked {
        let answer = crate::menus::take_box_answer().or_else(|| (!crate::menus::open()).then_some(false));
        if let Some(yes) = answer {
            eprintln!("[Update] {asked:?} asked: {}", if yes { "yes" } else { "no" });
            u.asked = None;
            match (asked, yes) {
                (Asked::Download, true) => {
                    u.downloading = u.updater.download();
                }
                (Asked::Retry, true) => {
                    u.retry = true;
                    u.updater.check();
                }
                (Asked::Quit, true) => {
                    eprintln!("[Update] quitting so the next start runs the update");
                    crate::settings::save();
                    exit.write(AppExit::Success);
                }
                (Asked::Download | Asked::Retry, false) => u.declined = true,
                (Asked::Quit, false) => {}
            }
        }
        // While the box is up, nothing else is asked.
        return;
    }

    if u.polled < POLL_EVERY {
        return;
    }
    u.polled = 0.0;
    u.updater.reap();
    let state = u.updater.state();
    let installed = u.updater.settings().installed.clone();
    let logged = match &state {
        State::Idle => String::new(),
        State::Checking => format!("checking for a newer release than {installed}"),
        State::UpToDate => format!("{installed} is the newest release"),
        State::Available { offer } => format!("{} is available ({})", offer.version, megabytes(offer.size)),
        State::Downloading { offer, .. } => format!("downloading {}", offer.version),
        State::Installed { offer } => format!("{} is installed: the next start runs it", offer.version),
        State::Failed(why) => format!("failed: {why}"),
        State::Cancelled => "stopped".into(),
    };
    if !logged.is_empty() && logged != u.logged {
        eprintln!("[Update] {logged}");
        u.logged = logged;
    }

    let mut line = String::new();
    match &state {
        State::Idle if u.since >= CHECK_AFTER => {
            u.updater.check();
        }
        State::Available { offer } if u.retry => {
            u.retry = false;
            u.downloading = u.updater.download();
            line = format!("Downloading DX RUST {}...", offer.version);
        }
        // The box doesn't grow with its text (MenuUIMessageBoxWindow's client size): a few lines, so it stays short.
        State::Available { offer } if !u.declined && crate::menus::open() => {
            let text = format!(
                "DX RUST {} is out ({}). Download it now? You can play on while it downloads; it starts next launch.",
                offer.version,
                megabytes(offer.size),
            );
            u.ask(Asked::Download, "Update Available", &text);
        }
        State::Downloading { offer, done } => {
            line = format!(
                "Downloading DX RUST {}: {}% ({} of {})",
                offer.version,
                done * 100 / offer.size.max(1),
                megabytes(*done),
                megabytes(offer.size),
            );
        }
        State::Installed { offer } => {
            if !u.told || u.result_left > 0.0 {
                line = format!("DX RUST {} is installed: it starts the next time you play.", offer.version);
            }
            if !u.told && crate::menus::open() {
                let text = format!("DX RUST {} is installed and starts next launch. Quit now?", offer.version);
                u.ask(Asked::Quit, "Update Installed", &text);
                u.told = true;
                u.result_left = RESULT_SHOWN;
            }
        }
        // A failed check is only logged, unless the player asked for the update (a download, or another go): then the
        // line says so until a menu is up to say why.
        State::Failed(why) if u.downloading || u.retry => {
            line = "The update didn't finish (open the menu for why); this version is unchanged.".into();
            if crate::menus::open() {
                // The whole reason is in log.txt; a path or URL in it would run out of the box.
                let mut why = why.clone();
                if why.chars().count() > 60 {
                    why = why.chars().take(57).chain("...".chars()).collect();
                }
                let text = format!("The update didn't finish: {why}. This version is unchanged. Try again?");
                u.ask(Asked::Retry, "Update Failed", &text);
                u.downloading = false;
                u.retry = false;
            }
        }
        _ => {}
    }
    let want = if line.is_empty() { Visibility::Hidden } else { Visibility::Inherited };
    if *visibility != want {
        *visibility = want;
    }
    if text.0 != line {
        text.0 = line;
    }
}
