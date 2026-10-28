//! TUI application state and command resolution.
//!
//! Terminal-independent and unit-testable: key handling, modal state machines,
//! and the resolution of user actions into [`ResolvedCommand`]s.  The
//! `ResolvedCommand` enum is turned into actual async jobs by the event loop
//! in `mod.rs`.

use std::path::Path;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::error::Result;
use crate::types::package_version::BumpOperation;
use crate::types::package_version::RustcPackageVersion;
use crate::types::params::TarballParams;
use crate::types::ubuntu::UbuntuRelease;
use crate::types::versions::RustVersion;

// ---------------------------------------------------------------------------
// Actions a key press can produce
// ---------------------------------------------------------------------------

/// What [`App::handle_key`] asks the event loop to do.
#[derive(Debug)]
pub enum AppAction {
    /// Nothing to do (purely a UI state change, already applied).
    None,
    /// Quit the TUI.
    Quit,
    /// Spawn a command. `description` is shown in the busy bar.
    Run {
        description: String,
        command: ResolvedCommand,
    },
}

/// A command the user triggered from the TUI, resolved into concrete
/// arguments.  The event loop turns this into an async job.
#[derive(Debug)]
pub enum ResolvedCommand {
    /// `sbuild` the package for the given release adjective.
    Build { release: String },
    /// `dpkg-buildpackage -S -I -i -nc -d -sa`.
    SourceBuild,
    /// Run lintian on the repo directory.
    Lintian,
    /// Rewrite the changelog top entry version via a bump operation.
    Bump { operation: BumpOperation },
    /// Rewrite the changelog top entry version to an explicit version string.
    SetVersion { version: String },
    /// Download / generate / overlay source tarballs.
    Tarball {
        params: TarballParams,
        target: crate::commands::tarball::TarballTarget,
    },
    /// `git fetch --all`.
    GitFetch,
}

/// Tarball target(s), TUI-local mirror of
/// `commands::tarball::TarballTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballTarget {
    Orig,
    Vendor,
    All,
}

impl TarballTarget {
    /// Label used in menu entries.
    fn label(self) -> &'static str {
        match self {
            TarballTarget::Orig => "orig",
            TarballTarget::Vendor => "vendor",
            TarballTarget::All => "all",
        }
    }
}

/// Tarball workflow action, TUI-local mirror of
/// `types::params::TarballAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballAction {
    Download,
    Generate,
    Overlay,
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

/// Which menu is open and what confirming it does.
#[derive(Debug, Clone)]
pub enum MenuKind {
    /// Bump-version operations; the selected index maps to an action via
    /// [`menu_bump_operation`].
    Bump,
    /// Tarball actions; each option carries its `(action, target)` pair.
    Tarball {
        actions: Vec<(TarballAction, TarballTarget)>,
    },
}

/// A menu (list selection) modal.
#[derive(Debug)]
pub struct MenuModal {
    /// Title shown in the popup.
    pub title: String,
    /// Selectable entries.
    pub options: Vec<String>,
    /// Highlighted entry.
    pub selected: usize,
    /// Which menu this is (drives what Enter does).
    pub kind: MenuKind,
}

/// A text-input modal.
#[derive(Debug)]
pub struct InputModal {
    /// Title / prompt shown in the popup.
    pub title: String,
    /// What to do when the value is submitted.
    pub on_submit: InputSpec,
    /// Current text (single-line; backspace-only editing in v1).
    pub value: String,
    /// Validation error shown above the input; the modal stays open.
    pub error: Option<String>,
}

/// What a submitted input value is used for.
#[derive(Debug, Clone)]
pub enum InputSpec {
    /// Target release for `sbuild` (Ubuntu adjective, e.g. `noble`).
    SbuildRelease,
    /// Explicit version string for the changelog top entry.
    SetVersion,
    /// New upstream version for the patch-release bump.
    BumpPatchUpstream,
    /// Series / release adjective for a backport or retarget bump; the
    /// carried `BumpOperation` marks which operation it belongs to.
    BumpSeries { marker: BumpOperation },
    /// Rust version for a tarball action.
    TarballVersion { spec: TarballPendingSpec },
    /// Ubuntu release adjective for a tarball action (may be empty).
    TarballSeries { spec: TarballPendingSpec },
    /// Confirm a destructive tarball flag (`--force` / `--replace`).
    TarballFlag { spec: TarballPendingSpec },
}

/// A tarball invocation collected incrementally by the wizard.
#[derive(Debug, Clone)]
pub struct TarballPendingSpec {
    pub action: TarballAction,
    pub target: TarballTarget,
    pub version: Option<String>,
    pub series: Option<String>,
    pub flag: bool,
}

/// The TUI's interaction mode.
#[derive(Debug)]
pub enum Mode {
    /// Normal browsing / command keys active.
    Normal,
    /// A selection menu is open.
    Menu(MenuModal),
    /// A text-input modal is open.
    Input(InputModal),
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Info-pane contents, re-read on refresh.
#[derive(Debug, Default, Clone)]
pub struct InfoPane {
    /// Source package name from the changelog first line.
    pub package: Option<String>,
    /// Raw version string.
    pub version: Option<String>,
    /// Parsed upstream version.
    pub upstream: Option<String>,
    /// Parsed series (backport target), if any.
    pub series: Option<String>,
    /// Ubuntu revision number.
    pub ubuntu_revision: Option<u32>,
    /// PPA suffix, if any.
    pub ppa: Option<u32>,
    /// Current git branch.
    pub branch: Option<String>,
    /// Repo directory.
    pub repo_dir: PathBuf,
    /// Problem detected while reading the info.
    pub error: Option<String>,
}

impl InfoPane {
    /// Render the info pane as lines.
    pub fn display_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("  repo: {}", self.repo_dir.display())];
        if let Some(e) = &self.error {
            lines.push(format!("  error: {e}"));
            return lines;
        }
        let version = self
            .version
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| "(unparsable)".to_owned());
        lines.push(format!(
            "  package: {}",
            self.package.as_deref().unwrap_or("?")
        ));
        lines.push(format!("  version: {version}"));
        if let (Some(upstream), Some(series), Some(rev)) =
            (&self.upstream, &self.series, self.ubuntu_revision)
        {
            lines.push(format!(
                "    upstream {upstream}, series {series}, 0ubuntu{rev}"
            ));
            if let Some(ppa) = self.ppa {
                lines.push(format!("    ppa suffix: ~ppa{ppa}"));
            }
        }
        lines.push(format!(
            "  branch: {}",
            self.branch.as_deref().unwrap_or("(unknown)")
        ));
        lines
    }
}

/// The TUI application state.
pub struct App {
    /// Repo directory.
    pub repo_dir: PathBuf,
    /// Info-pane contents.
    pub info: InfoPane,
    /// Log lines (oldest first, capped).
    pub log: Vec<String>,
    /// Lines scrolled back from the tail (0 = follow the tail).
    pub log_scroll: usize,
    /// Description of the running command, when one is running.
    pub busy: Option<String>,
    /// Summary of the last completed command.
    pub last_status: Option<String>,
    /// Set when the user asked to quit.
    pub should_quit: bool,
    /// Last sbuild release used (prefills the next build prompt).
    pub last_release: Option<String>,
    /// Current UI mode: normal key handling, or a modal menu/input.
    pub mode: Mode,
}

/// Log buffer cap (drop oldest beyond this).
pub const LOG_CAP: usize = 10_000;

/// Lines scrolled by one PageUp/PageDown.
pub const LOG_SCROLL_STEP: usize = 10;

impl App {
    /// Create the app and read the initial info pane.
    pub fn new(repo_dir: &Path) -> Self {
        let mut app = Self {
            repo_dir: repo_dir.to_path_buf(),
            info: InfoPane::default(),
            log: Vec::new(),
            log_scroll: 0,
            busy: None,
            last_status: None,
            should_quit: false,
            last_release: None,
            mode: Mode::Normal,
        };
        app.refresh();
        app
    }

    /// Append a line to the log (newlines split), respecting the scroll pin
    /// and the buffer cap.
    pub fn push_log(&mut self, line: &str) {
        for sub in line.split('\n') {
            if sub.is_empty() {
                continue;
            }
            self.log.push(sub.to_owned());
            if self.log.len() > LOG_CAP {
                let excess = self.log.len() - LOG_CAP;
                self.log.drain(..excess);
                // Keep the same content visible: the window shifts up too.
                self.log_scroll = self.log_scroll.saturating_sub(excess);
            }
        }
        if self.log_scroll > 0 {
            // Keep showing the same content while output streams.
            self.log_scroll = (self.log_scroll + 1).min(self.log.len());
        }
    }

    /// Re-read the changelog and git branch into the info pane.
    pub fn refresh(&mut self) {
        let changelog = self.repo_dir.join("debian/changelog");
        let mut info = InfoPane {
            repo_dir: self.repo_dir.clone(),
            ..InfoPane::default()
        };

        match crate::steps::changelog::read_current_version(&changelog) {
            Ok(version) => {
                info.version = Some(version.clone());
                match RustcPackageVersion::parse(&version) {
                    Ok(v) => {
                        // Package name: strip the version from the first line.
                        let first_line = std::fs::read_to_string(&changelog)
                            .ok()
                            .and_then(|c| c.lines().next().map(str::to_owned))
                            .unwrap_or_default();
                        info.package = first_line
                            .split_once(' ')
                            .map(|(name, _)| name.trim().to_owned());
                        info.upstream = Some(v.upstream.to_string());
                        info.series = v.series.clone();
                        info.ubuntu_revision = Some(v.ubuntu_revision);
                        info.ppa = v.ppa;
                    }
                    Err(e) => info.error = Some(e.to_string()),
                }
            }
            Err(e) => info.error = Some(e.to_string()),
        }
        info.branch = git_branch(&self.repo_dir);
        self.info = info;
    }

    /// Handle a key press. Returns the action the event loop should take.
    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return AppAction::Quit;
        }
        if key.kind == KeyEventKind::Release {
            return AppAction::None;
        }

        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Menu(_) => self.handle_menu_key(key),
            Mode::Input(_) => self.handle_input_key(key),
        }
    }

    /// Keys handled in normal mode.
    fn handle_normal_key(&mut self, key: KeyEvent) -> AppAction {
        // Log scrolling works at any time.
        match key.code {
            KeyCode::PageUp => {
                self.log_scroll = (self.log_scroll + LOG_SCROLL_STEP).min(self.log.len());
                return AppAction::None;
            }
            KeyCode::PageDown => {
                self.log_scroll = self.log_scroll.saturating_sub(LOG_SCROLL_STEP);
                return AppAction::None;
            }
            KeyCode::Up => {
                self.log_scroll = (self.log_scroll + 1).min(self.log.len());
                return AppAction::None;
            }
            KeyCode::Down => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
                return AppAction::None;
            }
            KeyCode::End => {
                self.log_scroll = 0;
                return AppAction::None;
            }
            _ => {}
        }

        // Command keys only when idle.
        if self.busy.is_some() {
            if key.code == KeyCode::Char('q') {
                self.push_log(
                    "  a command is still running — wait for it to finish before quitting",
                );
            }
            return AppAction::None;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                AppAction::Quit
            }
            KeyCode::Char('b') => {
                let default = self.last_release.clone().unwrap_or_default();
                self.open_input(
                    "sbuild release (Ubuntu adjective, e.g. noble)",
                    InputSpec::SbuildRelease,
                    default,
                );
                AppAction::None
            }
            KeyCode::Char('S') => {
                self.push_log("running dpkg-buildpackage -S ...");
                AppAction::Run {
                    description: "source build".to_owned(),
                    command: ResolvedCommand::SourceBuild,
                }
            }
            KeyCode::Char('l') => {
                self.push_log("running lintian ...");
                AppAction::Run {
                    description: "lintian".to_owned(),
                    command: ResolvedCommand::Lintian,
                }
            }
            KeyCode::Char('v') => {
                self.open_bump_menu();
                AppAction::None
            }
            KeyCode::Char('V') => {
                let default = self.info.version.clone().unwrap_or_default();
                self.open_input(
                    "set version (full rustc package version string)",
                    InputSpec::SetVersion,
                    default,
                );
                AppAction::None
            }
            KeyCode::Char('t') => {
                self.open_tarball_menu();
                AppAction::None
            }
            KeyCode::Char('g') => AppAction::Run {
                description: "git fetch --all".to_owned(),
                command: ResolvedCommand::GitFetch,
            },
            KeyCode::Char('R') => {
                self.refresh();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    /// Keys handled while a menu modal is open.
    fn handle_menu_key(&mut self, key: KeyEvent) -> AppAction {
        let (kind, selected, option_count) = match &self.mode {
            Mode::Menu(m) => (m.kind.clone(), m.selected, m.options.len()),
            _ => return AppAction::None,
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Mode::Menu(m) = &mut self.mode {
                    m.selected = m.selected.saturating_sub(1);
                }
                AppAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let Mode::Menu(m) = &mut self.mode else {
                    return AppAction::None;
                };
                if m.selected + 1 < option_count {
                    m.selected += 1;
                }
                AppAction::None
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                AppAction::None
            }
            KeyCode::Enter => match kind {
                MenuKind::Bump => self.activate_bump_menu_entry(selected),
                MenuKind::Tarball { actions } => {
                    let (action, target) = actions[selected];
                    self.advance_tarball_flow(TarballPendingSpec {
                        action,
                        target,
                        version: None,
                        series: None,
                        flag: false,
                    })
                }
            },
            _ => AppAction::None,
        }
    }

    /// Confirm an entry in the bump menu.
    fn activate_bump_menu_entry(&mut self, selected: usize) -> AppAction {
        match menu_bump_operation(selected) {
            MenuEntry::Run(operation) => {
                self.mode = Mode::Normal;
                self.push_log(&format!("bumping version: {operation:?} ..."));
                AppAction::Run {
                    description: "bump version".to_owned(),
                    command: ResolvedCommand::Bump { operation },
                }
            }
            MenuEntry::Input(spec) => {
                let (title, default) = match &spec {
                    InputSpec::BumpPatchUpstream => ("new upstream version (X.Y.Z)", String::new()),
                    InputSpec::BumpSeries { .. } => (
                        "target series (e.g. 24.04) or release adjective (e.g. noble)",
                        String::new(),
                    ),
                    _ => ("value", String::new()),
                };
                self.open_input(title, spec, default);
                AppAction::None
            }
        }
    }

    /// Keys handled while an input modal is open.
    fn handle_input_key(&mut self, key: KeyEvent) -> AppAction {
        if key.code == KeyCode::Esc {
            self.mode = Mode::Normal;
            return AppAction::None;
        }
        match key.code {
            KeyCode::Backspace => {
                if let Mode::Input(input) = &mut self.mode {
                    input.value.pop();
                    input.error = None;
                }
                AppAction::None
            }
            KeyCode::Enter => {
                let (spec, value) = match &self.mode {
                    Mode::Input(input) => (input.on_submit.clone(), input.value.clone()),
                    _ => return AppAction::None,
                };
                self.submit_input(spec, value.trim().to_owned())
            }
            KeyCode::Char(c) => {
                if let Mode::Input(input) = &mut self.mode {
                    input.value.push(c);
                    input.error = None;
                }
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    // -- modal helpers ----------------------------------------------------

    /// Open a text-input modal.
    fn open_input(&mut self, title: &str, spec: InputSpec, default: String) {
        self.mode = Mode::Input(InputModal {
            title: title.to_owned(),
            on_submit: spec,
            value: default,
            error: None,
        });
    }

    /// Open the bump-version menu.
    fn open_bump_menu(&mut self) {
        self.mode = Mode::Menu(MenuModal {
            title: "Bump version".to_owned(),
            options: bump_menu_options(),
            selected: 0,
            kind: MenuKind::Bump,
        });
    }

    /// Open the tarball menu (9 download/generate/overlay x orig/vendor/all).
    fn open_tarball_menu(&mut self) {
        let mut options = Vec::new();
        let mut actions = Vec::new();
        for action in [
            TarballAction::Download,
            TarballAction::Generate,
            TarballAction::Overlay,
        ] {
            for target in [
                TarballTarget::Orig,
                TarballTarget::Vendor,
                TarballTarget::All,
            ] {
                options.push(format!(
                    "{} {}",
                    tarball_action_label(action),
                    target.label()
                ));
                actions.push((action, target));
            }
        }
        self.mode = Mode::Menu(MenuModal {
            title: "Tarballs".to_owned(),
            options,
            selected: 0,
            kind: MenuKind::Tarball { actions },
        });
    }

    /// Advance the tarball wizard to the next missing input, or run.
    fn advance_tarball_flow(&mut self, spec: TarballPendingSpec) -> AppAction {
        if spec.version.is_none() {
            let default = self.info.upstream.clone().unwrap_or_default();
            self.open_input(
                "tarball rust version (X.Y.Z)",
                InputSpec::TarballVersion { spec },
                default,
            );
            return AppAction::None;
        }
        if spec.series.is_none() {
            let default = self
                .info
                .series
                .as_deref()
                .and_then(release_from_series)
                .unwrap_or_default();
            self.open_input(
                "tarball series adjective (empty = plain update naming)",
                InputSpec::TarballSeries { spec },
                default,
            );
            return AppAction::None;
        }
        let needs_flag = matches!(
            (spec.action, spec.target),
            (TarballAction::Generate, _)
                | (
                    TarballAction::Overlay,
                    TarballTarget::Vendor | TarballTarget::All
                )
        );
        if needs_flag && !spec.flag {
            let question = if spec.action == TarballAction::Generate {
                "overwrite an existing tarball? (y/N)"
            } else {
                "clean-replace vendor/ before overlay? (y/N)"
            };
            self.open_input(question, InputSpec::TarballFlag { spec }, String::new());
            return AppAction::None;
        }
        self.run_tarball(spec)
    }

    /// Validate the collected tarball spec and return the run action.
    fn run_tarball(&mut self, spec: TarballPendingSpec) -> AppAction {
        let Some(version) = spec.version.clone() else {
            return AppAction::None;
        };
        if version.is_empty() {
            self.set_input_error("a rust version is required");
            return AppAction::None;
        }
        let series = spec.series.as_deref().filter(|s| !s.is_empty());
        let params = match build_tarball_params(spec.action, &version, series, spec.flag) {
            Ok(p) => p,
            Err(e) => {
                self.set_input_error(&e.to_string());
                return AppAction::None;
            }
        };
        let target_label = spec.target.label();
        self.mode = Mode::Normal;
        self.push_log(&format!(
            "tarball {} {target_label} ...",
            tarball_action_label(spec.action)
        ));
        AppAction::Run {
            description: format!(
                "tarball {} {target_label}",
                tarball_action_label(spec.action)
            ),
            command: ResolvedCommand::Tarball {
                params,
                target: tarball_target_to_command(spec.target),
            },
        }
    }

    /// Set the validation error on the open input modal.
    fn set_input_error(&mut self, message: &str) {
        if let Mode::Input(input) = &mut self.mode {
            input.error = Some(message.to_owned());
        }
    }

    /// Submit an input value: validate and either run a command or continue
    /// the wizard.
    fn submit_input(&mut self, spec: InputSpec, value: String) -> AppAction {
        match spec {
            InputSpec::SbuildRelease => {
                if let Err(e) = UbuntuRelease::parse(&value) {
                    self.set_input_error(&e.to_string());
                    return AppAction::None;
                }
                self.mode = Mode::Normal;
                self.last_release = Some(value.clone());
                self.push_log(&format!("running sbuild -Ad{value} ..."));
                AppAction::Run {
                    description: format!("sbuild ({value})"),
                    command: ResolvedCommand::Build { release: value },
                }
            }
            InputSpec::SetVersion => {
                if let Err(e) = RustcPackageVersion::parse(&value) {
                    self.set_input_error(&e.to_string());
                    return AppAction::None;
                }
                self.mode = Mode::Normal;
                self.push_log(&format!("setting version to {value} ..."));
                AppAction::Run {
                    description: "set version".to_owned(),
                    command: ResolvedCommand::SetVersion { version: value },
                }
            }
            InputSpec::BumpPatchUpstream => {
                if let Err(e) = RustVersion::parse(&value) {
                    self.set_input_error(&e.to_string());
                    return AppAction::None;
                }
                self.mode = Mode::Normal;
                let operation = BumpOperation::PatchRelease {
                    upstream: value.clone(),
                };
                self.push_log(&format!("bumping version: {operation:?} ..."));
                AppAction::Run {
                    description: "bump version".to_owned(),
                    command: ResolvedCommand::Bump { operation },
                }
            }
            InputSpec::BumpSeries { marker } => {
                // Accept either a numeric series (e.g. "24.04") or an Ubuntu
                // release adjective (e.g. "noble").
                let (series, release): (Option<String>, Option<String>) = if value.contains('.') {
                    (Some(value), None)
                } else if let Err(e) = UbuntuRelease::parse(&value) {
                    self.set_input_error(&e.to_string());
                    return AppAction::None;
                } else {
                    (None, Some(value))
                };
                self.mode = Mode::Normal;
                let operation = match marker {
                    BumpOperation::Backport { .. } => BumpOperation::Backport { series, release },
                    _ => BumpOperation::Retarget { series, release },
                };
                self.push_log(&format!("bumping version: {operation:?} ..."));
                AppAction::Run {
                    description: "bump version".to_owned(),
                    command: ResolvedCommand::Bump { operation },
                }
            }
            InputSpec::TarballVersion { spec } => {
                if let Err(e) = RustVersion::parse(&value) {
                    self.set_input_error(&e.to_string());
                    return AppAction::None;
                }
                let mut spec = spec;
                spec.version = Some(value);
                self.advance_tarball_flow(spec)
            }
            InputSpec::TarballSeries { spec } => {
                // Empty input = plain update naming (no series suffix).
                let mut spec = spec;
                spec.series = Some(value);
                self.advance_tarball_flow(spec)
            }
            InputSpec::TarballFlag { spec } => {
                let mut spec = spec;
                spec.flag = matches!(value.as_str(), "y" | "Y" | "yes" | "Yes");
                self.advance_tarball_flow(spec)
            }
        }
    }

    /// Record a completed command.
    pub fn on_command_done(&mut self, description: &str, result: Result<String>) {
        match result {
            Ok(summary) => {
                self.last_status = Some(format!("{description}: {summary}"));
                self.push_log(&format!("== {description}: {summary}"));
            }
            Err(e) => {
                self.last_status = Some(format!("{description} FAILED"));
                self.push_log(&format!("== {description} FAILED: {e}"));
            }
        }
        self.refresh();
    }
}

// ---------------------------------------------------------------------------
// Shared pure helpers
// ---------------------------------------------------------------------------

/// One selectable entry of the bump menu.
#[derive(Debug)]
pub enum MenuEntry {
    /// Run this bump operation immediately.
    Run(BumpOperation),
    /// Open a follow-up input modal first.
    Input(InputSpec),
}

/// Map a bump-menu selection index to its action.
pub fn menu_bump_operation(index: usize) -> MenuEntry {
    match index {
        0 => MenuEntry::Input(InputSpec::BumpPatchUpstream),
        1 => MenuEntry::Run(BumpOperation::UbuntuRevision),
        2 => MenuEntry::Run(BumpOperation::Repack),
        3 => MenuEntry::Input(InputSpec::BumpSeries {
            marker: BumpOperation::Backport {
                series: None,
                release: None,
            },
        }),
        4 => MenuEntry::Run(BumpOperation::BackportRevision),
        5 => MenuEntry::Run(BumpOperation::BackportRepack),
        6 => MenuEntry::Input(InputSpec::BumpSeries {
            marker: BumpOperation::Retarget {
                series: None,
                release: None,
            },
        }),
        7 => MenuEntry::Run(BumpOperation::Ppa),
        8 => MenuEntry::Run(BumpOperation::ClearPpa),
        _ => MenuEntry::Run(BumpOperation::UbuntuRevision),
    }
}

/// The bump-menu option labels (index-aligned with
/// [`menu_bump_operation`]).
pub fn bump_menu_options() -> Vec<String> {
    vec![
        "patch release — new upstream X.Y.Z (resets all fields)".to_owned(),
        "ubuntu revision (0ubuntuN -> 0ubuntuN+1)".to_owned(),
        "repack (+dfsg -> +dfsg1...)".to_owned(),
        "backport — add/replace ~series suffix".to_owned(),
        "backport revision (~series.N -> ~series.N+1)".to_owned(),
        "backport repack (~series -> ~series.1...)".to_owned(),
        "retarget — move backport to another series".to_owned(),
        "ppa suffix (add/increment ~ppaN)".to_owned(),
        "clear ppa suffix (final archive upload)".to_owned(),
    ]
}

/// Label for a tarball action.
pub fn tarball_action_label(action: TarballAction) -> &'static str {
    match action {
        TarballAction::Download => "download",
        TarballAction::Generate => "generate",
        TarballAction::Overlay => "overlay",
    }
}

/// Build `TarballParams` from the wizard's collected values.
///
/// `series` is an Ubuntu release adjective (e.g. `noble`); `None` selects the
/// plain update naming. `flag` means `--force` for generate and `--replace`
/// for vendor/all overlays.
pub fn build_tarball_params(
    action: TarballAction,
    rust_version: &str,
    series: Option<&str>,
    flag: bool,
) -> Result<TarballParams> {
    let (action, force, overlay_replace) = match action {
        TarballAction::Download => (crate::types::params::TarballAction::Download, false, false),
        TarballAction::Generate => (crate::types::params::TarballAction::Generate, flag, false),
        TarballAction::Overlay => (crate::types::params::TarballAction::Overlay, false, flag),
    };
    TarballParams::new(action, rust_version, series, force, overlay_replace)
}

/// Convert the TUI-local tarball target to the command-layer enum.
fn tarball_target_to_command(target: TarballTarget) -> crate::commands::tarball::TarballTarget {
    match target {
        TarballTarget::Orig => crate::commands::tarball::TarballTarget::Orig,
        TarballTarget::Vendor => crate::commands::tarball::TarballTarget::Vendor,
        TarballTarget::All => crate::commands::tarball::TarballTarget::All,
    }
}

/// Best-effort release adjective for a numeric series (`"24.04"` → `"noble"`).
fn release_from_series(series: &str) -> Option<String> {
    UbuntuRelease::release_from_series(series).map(str::to_owned)
}

/// Current git branch name, or `None` when git fails.
fn git_branch(repo_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if name.is_empty() { None } else { Some(name) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn scratch_app() -> App {
        let dir = std::env::temp_dir().join(format!(
            "thermite-tui-app-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("debian")).unwrap();
        std::fs::write(
            dir.join("debian/changelog"),
            "rustc-1.95 (1.95.0+dfsg-0ubuntu1) plucky; urgency=medium\n\n  * test\n\n -- T <t@t>  Mon, 01 Jan 2026 00:00:00 +0000\n",
        )
        .unwrap();
        App::new(&dir)
    }

    #[test]
    fn push_log_splits_and_caps() {
        let mut app = App::new(Path::new("/nonexistent"));
        app.push_log("one\n\ntwo");
        assert_eq!(app.log, vec!["one", "two"]);
        assert_eq!(app.log_scroll, 0);
    }

    #[test]
    fn log_scroll_follows_and_pins() {
        let mut app = App::new(Path::new("/nonexistent"));
        for i in 0..30 {
            app.push_log(&format!("line {i}"));
        }
        assert_eq!(app.log_scroll, 0);
        // Scroll back 5 lines; streaming output keeps the same content visible.
        for _ in 0..5 {
            app.handle_key(key(KeyCode::Up));
        }
        assert_eq!(app.log_scroll, 5);
        app.push_log("new line");
        assert_eq!(app.log_scroll, 6);
        // End returns to follow mode.
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.log_scroll, 0);
    }

    #[test]
    fn ctrl_c_quits_from_any_mode() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('v')));
        assert!(matches!(app.mode, Mode::Menu(_)));
        let mut ctrl_c = key(KeyCode::Char('c'));
        ctrl_c.modifiers = KeyModifiers::CONTROL;
        assert!(matches!(app.handle_key(ctrl_c), AppAction::Quit));
    }

    #[test]
    fn q_blocked_while_busy() {
        let mut app = scratch_app();
        // Give the log some content so PageUp has room to scroll.
        for i in 0..30 {
            app.push_log(&format!("line {i}"));
        }
        app.busy = Some("sbuild (noble)".to_owned());
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('q'))),
            AppAction::None
        ));
        assert!(!app.should_quit);
        // Log scrolling still works while busy.
        assert!(matches!(
            app.handle_key(key(KeyCode::PageUp)),
            AppAction::None
        ));
        assert_eq!(app.log_scroll, LOG_SCROLL_STEP);
    }

    #[test]
    fn build_flow_validates_release_adjective() {
        let mut app = scratch_app();
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('b'))),
            AppAction::None
        ));
        let Mode::Input(m) = &app.mode else {
            panic!("expected input modal");
        };
        assert!(matches!(m.on_submit, InputSpec::SbuildRelease));
        // Invalid adjective keeps the modal open with an error.
        let spec = m.on_submit.clone();
        assert!(matches!(
            app.submit_input(spec, "not-a-release".to_owned()),
            AppAction::None
        ));
        let Mode::Input(m) = &app.mode else {
            panic!("expected input modal");
        };
        assert!(m.error.is_some());
        // Valid adjective → run.
        let spec = m.on_submit.clone();
        match app.submit_input(spec, "noble".to_owned()) {
            AppAction::Run {
                description,
                command,
            } => {
                assert_eq!(description, "sbuild (noble)");
                assert!(matches!(command, ResolvedCommand::Build { .. }));
                assert_eq!(app.last_release.as_deref(), Some("noble"));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn bump_menu_maps_entries() {
        assert!(matches!(
            menu_bump_operation(0),
            MenuEntry::Input(InputSpec::BumpPatchUpstream)
        ));
        assert!(matches!(
            menu_bump_operation(1),
            MenuEntry::Run(BumpOperation::UbuntuRevision)
        ));
        assert!(matches!(
            menu_bump_operation(3),
            MenuEntry::Input(InputSpec::BumpSeries {
                marker: BumpOperation::Backport { .. }
            })
        ));
        assert!(matches!(
            menu_bump_operation(6),
            MenuEntry::Input(InputSpec::BumpSeries {
                marker: BumpOperation::Retarget { .. }
            })
        ));
        assert_eq!(bump_menu_options().len(), 9);
    }

    #[test]
    fn bump_flow_ubuntu_revision() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('v')));
        let Mode::Menu(m) = &app.mode else {
            panic!("expected menu");
        };
        assert_eq!(m.selected, 0);
        // Select "ubuntu revision" (index 1) and confirm.
        app.handle_key(key(KeyCode::Down));
        match app.handle_key(key(KeyCode::Enter)) {
            AppAction::Run {
                description,
                command,
            } => {
                assert_eq!(description, "bump version");
                assert!(matches!(
                    command,
                    ResolvedCommand::Bump {
                        operation: BumpOperation::UbuntuRevision
                    }
                ));
                assert!(matches!(app.mode, Mode::Normal));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn tarball_menu_has_nine_entries() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('t')));
        let Mode::Menu(m) = &app.mode else {
            panic!("expected menu");
        };
        assert_eq!(m.options.len(), 9);
        let MenuKind::Tarball { actions } = &m.kind else {
            panic!("expected tarball menu");
        };
        assert_eq!(actions[0], (TarballAction::Download, TarballTarget::Orig));
        assert_eq!(actions[8], (TarballAction::Overlay, TarballTarget::All));
    }

    #[test]
    fn tarball_wizard_full_flow_generate_orig() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('t')));
        // "generate orig" is index 3 (download orig/vendor/all = 0..2).
        for _ in 0..3 {
            app.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Enter));
        // Version input prefilled from the changelog upstream version.
        let Mode::Input(m) = &app.mode else {
            panic!("expected version input");
        };
        assert_eq!(m.value, "1.95.0");
        let InputSpec::TarballVersion { .. } = &m.on_submit else {
            panic!("expected tarball version input");
        };
        let spec = m.on_submit.clone();
        let action = app.submit_input(spec, "1.95.1".to_owned());
        assert!(matches!(action, AppAction::None));
        // Series input (prefilled empty for devel package).
        let Mode::Input(m) = &app.mode else {
            panic!("expected series input");
        };
        let InputSpec::TarballSeries { .. } = &m.on_submit else {
            panic!("expected tarball series input");
        };
        let spec = m.on_submit.clone();
        assert!(matches!(
            app.submit_input(spec, String::new()),
            AppAction::None
        ));
        // Force flag confirm (y/N).
        let Mode::Input(m) = &app.mode else {
            panic!("expected flag input");
        };
        let InputSpec::TarballFlag { .. } = &m.on_submit else {
            panic!("expected tarball flag input");
        };
        let spec = m.on_submit.clone();
        let action = app.submit_input(spec, "y".to_owned());
        match action {
            AppAction::Run {
                description,
                command,
            } => {
                assert!(description.contains("generate orig"));
                let ResolvedCommand::Tarball { params, target } = command else {
                    panic!("expected tarball command");
                };
                assert!(params.force);
                assert!(matches!(
                    target,
                    crate::commands::tarball::TarballTarget::Orig
                ));
                assert_eq!(params.rust_version.to_string(), "1.95.1");
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn tarball_wizard_download_skips_flag_step() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('t')));
        // "download vendor" is index 1.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        let Mode::Input(m) = &app.mode else {
            panic!("expected version input");
        };
        let InputSpec::TarballVersion { .. } = &m.on_submit else {
            panic!("expected tarball version input");
        };
        let spec = m.on_submit.clone();
        app.submit_input(spec, "1.95.0".to_owned());
        let Mode::Input(m) = &app.mode else {
            panic!("expected series input");
        };
        let InputSpec::TarballSeries { .. } = &m.on_submit else {
            panic!("expected tarball series input");
        };
        let spec = m.on_submit.clone();
        // Empty series, no flag confirm expected for downloads.
        match app.submit_input(spec, String::new()) {
            AppAction::Run { command, .. } => {
                let ResolvedCommand::Tarball { params, target } = command else {
                    panic!("expected tarball command");
                };
                assert!(!params.force);
                assert!(!params.overlay_replace);
                assert!(matches!(
                    target,
                    crate::commands::tarball::TarballTarget::Vendor
                ));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn set_version_validates() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('V')));
        let Mode::Input(m) = &app.mode else {
            panic!("expected input modal");
        };
        assert_eq!(m.value, "1.95.0+dfsg-0ubuntu1");
        let spec = m.on_submit.clone();
        // Invalid version keeps the modal open.
        assert!(matches!(
            app.submit_input(spec.clone(), "bogus".to_owned()),
            AppAction::None
        ));
        let Mode::Input(m) = &app.mode else {
            panic!("expected input modal");
        };
        assert!(m.error.is_some());
        let action = app.submit_input(spec, "1.96.0+dfsg-0ubuntu1".to_owned());
        match action {
            AppAction::Run { command, .. } => {
                assert!(matches!(
                    command,
                    ResolvedCommand::SetVersion { version } if version == "1.96.0+dfsg-0ubuntu1"
                ));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn build_tarball_params_backport_series() {
        let params =
            build_tarball_params(TarballAction::Generate, "1.95.0", Some("noble"), true).unwrap();
        assert!(params.force);
        assert_eq!(params.dfsg_suffix(), "~24.04");

        let params = build_tarball_params(TarballAction::Download, "1.95.0", None, false).unwrap();
        assert!(!params.force);
        assert_eq!(params.dfsg_suffix(), "");
    }

    #[test]
    fn refresh_reads_changelog_and_fields() {
        let mut app = scratch_app();
        app.refresh();
        assert_eq!(app.info.package.as_deref(), Some("rustc-1.95"));
        assert_eq!(app.info.version.as_deref(), Some("1.95.0+dfsg-0ubuntu1"));
        assert_eq!(app.info.upstream.as_deref(), Some("1.95.0"));
        assert_eq!(app.info.series, None);
        assert_eq!(app.info.ubuntu_revision, Some(1));
        let lines = app.info.display_lines();
        assert!(lines.iter().any(|l| l.contains("rustc-1.95")));
        assert!(lines.iter().any(|l| l.contains("1.95.0+dfsg-0ubuntu1")));
    }

    #[test]
    fn on_command_done_records_status_and_refreshes() {
        let mut app = scratch_app();
        app.on_command_done("bump version", Ok("1.95.0+dfsg-0ubuntu2".to_owned()));
        assert_eq!(
            app.last_status.as_deref(),
            Some("bump version: 1.95.0+dfsg-0ubuntu2")
        );
        assert!(
            app.log
                .iter()
                .any(|l| l.contains("== bump version: 1.95.0+dfsg-0ubuntu2"))
        );
        let err = crate::error::ThermiteError::CommandNotFound("sbuild".to_owned());
        app.on_command_done("sbuild (noble)", Err(err));
        assert_eq!(app.last_status.as_deref(), Some("sbuild (noble) FAILED"));
    }

    #[test]
    fn input_editing_and_escape() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('V')));
        for c in "1.96".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let Mode::Input(m) = &app.mode else {
            panic!("expected input modal");
        };
        assert!(m.value.ends_with("1.96"));
        app.handle_key(key(KeyCode::Backspace));
        let Mode::Input(m) = &app.mode else {
            panic!("expected input modal");
        };
        assert!(m.value.ends_with("1.9"));
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn menu_escape_and_navigation_clamps() {
        let mut app = scratch_app();
        app.handle_key(key(KeyCode::Char('t')));
        for _ in 0..20 {
            app.handle_key(key(KeyCode::Down));
        }
        let Mode::Menu(m) = &app.mode else {
            panic!("expected menu");
        };
        assert_eq!(m.selected, 8);
        for _ in 0..20 {
            app.handle_key(key(KeyCode::Up));
        }
        let Mode::Menu(m) = &app.mode else {
            panic!("expected menu");
        };
        assert_eq!(m.selected, 0);
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }
}
