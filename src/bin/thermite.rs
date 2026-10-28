use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use thermite::commands::tarball::TarballTarget;
use thermite::commands::{backport, tarball, update, version};
use thermite::error::{Result, ThermiteError};
use thermite::shell;
use thermite::types::params::{BackportParams, TarballAction, TarballParams, UpdateParams};

/// thermite — Ubuntu Rust toolchain packaging tool.
#[derive(Debug, Parser)]
#[command(name = "thermite", version, about)]
struct Cli {
    /// Increase output verbosity.
    ///
    /// Pass once (-v) to print each external command before it runs.
    /// Pass twice (-vv) to also show a concise explanation with documentation
    /// links at the start of every phase.
    #[arg(short = 'v', long, action = ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Package a new upstream Rust toolchain release for Ubuntu.
    Update {
        /// Full Rust version being packaged, in X.Y.Z format (e.g. 1.85.1).
        #[arg(short = 'u', long)]
        rust_update_version: String,

        /// Full Rust version being replaced, in X.Y.Z format (e.g. 1.84.0).
        #[arg(short = 'o', long)]
        rust_old_version: String,

        /// Target Ubuntu release adjective (e.g. noble).
        #[arg(short = 'r', long)]
        release: String,

        /// Launchpad username (also used as personal Git remote name).
        #[arg(short = 'l', long)]
        lpuser: String,

        /// Launchpad bug ID number for this work (digits only).
        #[arg(short = 'b', long)]
        lp_bug_number: String,

        /// Local Git remote name for the Foundations rustc repository.
        #[arg(short = 'g', long, default_value = "foundations")]
        git_remote: String,

        /// Path to the root of the Debian source package (defaults to the
        /// current working directory).
        #[arg(short = 'd', long)]
        repo_dir: Option<PathBuf>,
    },

    /// Backport an existing Rust toolchain package to an older Ubuntu release.
    Backport {
        /// Full Rust version to backport, in X.Y.Z format (e.g. 1.85.0).
        #[arg(short = 'u', long)]
        rust_version: String,

        /// Ubuntu release to backport FROM (e.g. noble, or 'devel' for the
        /// current Ubuntu development release).
        #[arg(short = 's', long)]
        source_release: String,

        /// Ubuntu release to backport TO (e.g. jammy).
        #[arg(short = 'r', long)]
        release: String,

        /// Launchpad username (also used as personal Git remote name).
        #[arg(short = 'l', long)]
        lpuser: String,

        /// Launchpad bug ID number for this backport (digits only).
        /// Omit for proactive backports that have no associated bug.
        #[arg(short = 'b', long)]
        lp_bug_number: Option<String>,

        /// Local Git remote name for the Foundations rustc repository.
        #[arg(short = 'g', long, default_value = "foundations")]
        git_remote: String,

        /// Path to the root of the Debian source package (defaults to the
        /// current working directory).
        #[arg(short = 'd', long)]
        repo_dir: Option<PathBuf>,

        /// Perform a dry run: skip all hard-to-revert operations (remote git
        /// push, PPA creation, dput uploads) and print what each would have
        /// done instead. All local operations (sbuild, lintian, etc.) still
        /// run normally.
        #[arg(long)]
        dry_run: bool,
    },

    /// Parse, explain, format, or bump Ubuntu rustc package version strings.
    Version {
        #[command(subcommand)]
        subcommand: VersionCommands,
    },

    /// Download or regenerate the orig and orig-vendor source tarballs.
    Tarball {
        #[command(subcommand)]
        action: TarballCommands,
    },
}

#[derive(Debug, Subcommand)]
enum VersionCommands {
    /// Parse a version string and display its components.
    ///
    /// INPUT can be a version string, a path to a directory containing
    /// debian/changelog, or a path to a changelog file.
    /// When omitted, uses the current directory.
    Parse {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Explain each component of a version string with documentation.
    ///
    /// INPUT can be a version string, a path to a directory containing
    /// debian/changelog, or a path to a changelog file.
    /// When omitted, uses the current directory.
    Explain {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,
    },

    /// Construct a version string from individual components.
    Format {
        /// Upstream Rust version in X.Y.Z format (required).
        #[arg(long)]
        upstream: String,

        /// Repack number (the N in +dfsgN). Omit for bare +dfsg.
        #[arg(long)]
        repack: Option<u32>,

        /// Target series number for backports (e.g. "24.04").
        /// Mutually exclusive with --release.
        #[arg(long)]
        series: Option<String>,

        /// Target Ubuntu release adjective (e.g. "noble").
        /// Resolves to the series number automatically.
        /// Mutually exclusive with --series.
        #[arg(long)]
        release: Option<String>,

        /// Backport repack number (requires --series or --release).
        #[arg(long)]
        backport_repack: Option<u32>,

        /// Mark as stage0 bootstrap (requires --series or --release).
        #[arg(long)]
        stage0: bool,

        /// Ubuntu revision number (default: 1).
        #[arg(long, default_value = "1")]
        ubuntu_revision: u32,

        /// Backport revision number (default: 1 when series is set).
        #[arg(long)]
        backport_revision: Option<u32>,

        /// PPA upload number. Omit for non-PPA (final archive) versions.
        #[arg(long)]
        ppa: Option<u32>,
    },

    /// Bump a version string according to a specified operation.
    Bump {
        #[command(subcommand)]
        operation: BumpOperationCmd,
    },
}

#[derive(Debug, Subcommand)]
enum BumpOperationCmd {
    /// New upstream patch release: resets all fields.
    PatchRelease {
        /// The new upstream version (e.g. "1.95.1").
        #[arg(long)]
        upstream: String,

        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Increment the Ubuntu revision number.
    UbuntuRevision {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Increment the repack number (resets Ubuntu revision to 1).
    Repack {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Generate a backport version (adds series, sets backport_revision=1).
    Backport {
        /// Target series number (e.g. "24.04").
        #[arg(long)]
        series: Option<String>,

        /// Target Ubuntu release adjective (e.g. "noble").
        #[arg(long)]
        release: Option<String>,

        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Increment the backport revision.
    BackportRevision {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Increment the backport repack (resets backport revision to 1).
    BackportRepack {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Retarget a backport to a different series.
    Retarget {
        /// New target series number (e.g. "22.04").
        #[arg(long)]
        series: Option<String>,

        /// New target Ubuntu release adjective (e.g. "jammy").
        #[arg(long)]
        release: Option<String>,

        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Increment the PPA number (for iterating on PPA builds).
    Ppa {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },

    /// Remove the PPA suffix (for final archive upload).
    ClearPpa {
        /// Version string, path to a package directory, or path to a
        /// debian/changelog file. Defaults to the current directory.
        input: Option<String>,

        /// Output the bumped version as JSON (includes all parsed fields).
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum TarballCommands {
    /// Download the tarball(s) from the staging PPA or the Ubuntu archive.
    Download {
        #[command(subcommand)]
        target: DownloadTarget,
    },
    /// Generate the tarball(s) locally (uscan or debian/rules vendor-tarball).
    Generate {
        #[command(subcommand)]
        target: GenerateTarget,
    },
    /// Extract an already-obtained tarball's contents into the repo directory.
    Overlay {
        #[command(subcommand)]
        target: OverlayTarget,
    },
}

/// Arguments shared by every `tarball` leaf.
#[derive(Debug, Args)]
struct TarballCommonArgs {
    /// Full Rust version in X.Y.Z format (e.g. 1.85.0).
    #[arg(short = 'u', long)]
    rust_version: String,

    /// Ubuntu release adjective used for backport-style tarball names
    /// (e.g. noble → '+dfsg~26.04'). Omit for plain update naming ('+dfsg').
    #[arg(long)]
    series: Option<String>,

    /// Path to the root of the Debian source package (defaults to the
    /// current working directory).
    #[arg(short = 'd', long)]
    repo_dir: Option<PathBuf>,
}

/// Download leaves (no `--force`: download reuses what exists, and the
/// working tree is never touched — use `tarball overlay` for that).
#[derive(Debug, Subcommand)]
enum DownloadTarget {
    /// The orig tarball (filtered upstream Rust source).
    Orig(TarballCommonArgs),
    /// The orig-vendor tarball (vendored crate dependencies).
    Vendor(TarballCommonArgs),
    /// Both tarballs, orig first.
    All(TarballCommonArgs),
}

/// Generate leaves (with `--force` to overwrite an existing tarball).
#[derive(Debug, Subcommand)]
enum GenerateTarget {
    /// The orig tarball (filtered upstream Rust source).
    Orig(TarballGenerateArgs),
    /// The orig-vendor tarball (vendored crate dependencies).
    Vendor(TarballGenerateArgs),
    /// Both tarballs, orig first.
    All(TarballGenerateArgs),
}

#[derive(Debug, Args)]
struct TarballGenerateArgs {
    #[command(flatten)]
    common: TarballCommonArgs,

    /// Overwrite the tarball if it already exists in the parent directory.
    #[arg(long)]
    force: bool,
}

/// Overlay leaves: extract a tarball that already exists in the parent
/// directory into the repo dir. No `--force`: overlay never creates tarballs.
#[derive(Debug, Subcommand)]
enum OverlayTarget {
    /// The orig tarball (filtered upstream Rust source), extracted with its
    /// top-level source directory stripped.
    Orig(TarballCommonArgs),
    /// The orig-vendor tarball (vendored crate dependencies).
    Vendor(TarballOverlayVendorArgs),
    /// Both tarballs, orig first.
    All(TarballOverlayVendorArgs),
}

#[derive(Debug, Args)]
struct TarballOverlayVendorArgs {
    #[command(flatten)]
    common: TarballCommonArgs,

    /// Remove the existing vendor/ directory before extracting the vendor
    /// tarball (clean replace) instead of merging over it. Applies to the
    /// vendor tarball only.
    #[arg(long)]
    replace: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Resolve the repo directory to an absolute canonical path.
fn resolve_repo_dir(repo_dir: Option<PathBuf>) -> Result<PathBuf> {
    // Canonicalize so that `repo_dir.parent().unwrap_or(repo_dir)` in
    // downstream steps resolves to the real parent directory even when a
    // relative path with no parent component (e.g. `.`) is passed:
    // `Path::new(".").parent()` returns `Some("")`, which would otherwise
    // resolve to the process cwd (the source tree itself) and cause
    // logs/tarballs to be written there.
    match repo_dir {
        Some(p) => std::fs::canonicalize(&p).map_err(ThermiteError::Io),
        None => std::env::current_dir().map_err(ThermiteError::Io),
    }
}

fn tarball_download_params(common: &TarballCommonArgs) -> Result<TarballParams> {
    TarballParams::new(
        TarballAction::Download,
        &common.rust_version,
        common.series.as_deref(),
        false,
        false,
    )
}

fn tarball_generate_params(common: &TarballCommonArgs, force: bool) -> Result<TarballParams> {
    TarballParams::new(
        TarballAction::Generate,
        &common.rust_version,
        common.series.as_deref(),
        force,
        false,
    )
}

fn tarball_overlay_params(common: &TarballCommonArgs, replace: bool) -> Result<TarballParams> {
    TarballParams::new(
        TarballAction::Overlay,
        &common.rust_version,
        common.series.as_deref(),
        false,
        replace,
    )
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    shell::set_verbosity(cli.verbose);

    match cli.command {
        Commands::Update {
            rust_update_version,
            rust_old_version,
            release,
            lpuser,
            lp_bug_number,
            git_remote,
            repo_dir,
        } => {
            let params = UpdateParams::new(
                &rust_update_version,
                &rust_old_version,
                &release,
                &lpuser,
                &git_remote,
                &lp_bug_number,
            )?;

            let repo_path = resolve_repo_dir(repo_dir)?;

            update::run(&params, &repo_path).await?;
        }

        Commands::Backport {
            rust_version,
            source_release,
            release,
            lpuser,
            lp_bug_number,
            git_remote,
            repo_dir,
            dry_run,
        } => {
            let params = BackportParams::new(
                &rust_version,
                &source_release,
                &release,
                &lpuser,
                &git_remote,
                lp_bug_number.as_deref(),
                dry_run,
            )?;

            let repo_path = resolve_repo_dir(repo_dir)?;

            backport::run(&params, &repo_path).await?;
        }

        Commands::Version { subcommand } => {
            dispatch_version(subcommand)?;
        }

        Commands::Tarball { action } => {
            let (params, repo_dir, target) = match action {
                TarballCommands::Download { target } => {
                    let (common, target) = match target {
                        DownloadTarget::Orig(args) => (args, TarballTarget::Orig),
                        DownloadTarget::Vendor(args) => (args, TarballTarget::Vendor),
                        DownloadTarget::All(args) => (args, TarballTarget::All),
                    };
                    (tarball_download_params(&common)?, common.repo_dir, target)
                }
                TarballCommands::Generate { target } => {
                    let (common, force, target) = match target {
                        GenerateTarget::Orig(args) => {
                            (args.common, args.force, TarballTarget::Orig)
                        }
                        GenerateTarget::Vendor(args) => {
                            (args.common, args.force, TarballTarget::Vendor)
                        }
                        GenerateTarget::All(args) => (args.common, args.force, TarballTarget::All),
                    };
                    let params = tarball_generate_params(&common, force)?;
                    (params, common.repo_dir, target)
                }
                TarballCommands::Overlay { target } => {
                    let (common, replace, target) = match target {
                        OverlayTarget::Orig(args) => (args, false, TarballTarget::Orig),
                        OverlayTarget::Vendor(args) => {
                            (args.common, args.replace, TarballTarget::Vendor)
                        }
                        OverlayTarget::All(args) => (args.common, args.replace, TarballTarget::All),
                    };
                    let params = tarball_overlay_params(&common, replace)?;
                    (params, common.repo_dir, target)
                }
            };

            let repo_path = resolve_repo_dir(repo_dir)?;

            tarball::run(&params, &repo_path, target).await?;
        }
    }

    Ok(())
}

/// Dispatch the `thermite version` subcommands.
///
/// These are all synchronous (no async I/O needed), so we handle them
/// directly without awaiting.
fn dispatch_version(cmd: VersionCommands) -> Result<()> {
    match cmd {
        VersionCommands::Parse { input, json } => {
            version::run_parse(input.as_deref(), json)?;
        }

        VersionCommands::Explain { input } => {
            version::run_explain(input.as_deref())?;
        }

        VersionCommands::Format {
            upstream,
            repack,
            series,
            release,
            backport_repack,
            stage0,
            ubuntu_revision,
            backport_revision,
            ppa,
        } => {
            version::run_format(&version::FormatParams {
                upstream: &upstream,
                repack_number: repack,
                series: series.as_deref(),
                release: release.as_deref(),
                backport_repack,
                stage0,
                ubuntu_revision,
                backport_revision,
                ppa,
            })?;
        }

        VersionCommands::Bump { operation } => {
            let (input, json, op) = convert_bump_operation(operation);
            version::run_bump(input.as_deref(), &op, json)?;
        }
    }
    Ok(())
}

/// Convert the CLI bump operation enum into the library's `BumpOperation`,
/// also returning the input source and JSON flag extracted from the variant.
fn convert_bump_operation(cmd: BumpOperationCmd) -> (Option<String>, bool, version::BumpOperation) {
    match cmd {
        BumpOperationCmd::PatchRelease {
            upstream,
            input,
            json,
        } => (
            input,
            json,
            version::BumpOperation::PatchRelease { upstream },
        ),
        BumpOperationCmd::UbuntuRevision { input, json } => {
            (input, json, version::BumpOperation::UbuntuRevision)
        }
        BumpOperationCmd::Repack { input, json } => (input, json, version::BumpOperation::Repack),
        BumpOperationCmd::Backport {
            series,
            release,
            input,
            json,
        } => (
            input,
            json,
            version::BumpOperation::Backport { series, release },
        ),
        BumpOperationCmd::BackportRevision { input, json } => {
            (input, json, version::BumpOperation::BackportRevision)
        }
        BumpOperationCmd::BackportRepack { input, json } => {
            (input, json, version::BumpOperation::BackportRepack)
        }
        BumpOperationCmd::Retarget {
            series,
            release,
            input,
            json,
        } => (
            input,
            json,
            version::BumpOperation::Retarget { series, release },
        ),
        BumpOperationCmd::Ppa { input, json } => (input, json, version::BumpOperation::Ppa),
        BumpOperationCmd::ClearPpa { input, json } => {
            (input, json, version::BumpOperation::ClearPpa)
        }
    }
}
