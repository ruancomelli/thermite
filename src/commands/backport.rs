use std::path::{Path, PathBuf};

use tracing::info;

use crate::error::{Result, ThermiteError};
use crate::shell;
use crate::steps::{build, changelog, compat, git, lintian, ppa, tarball_fetch, uscan, vendor};
use crate::types::params::BackportParams;
use crate::ui::{
    confirm_sink, print_info_box, print_phase_header, print_tool_checks, prompt_input,
    prompt_select,
};

/// Required external tools for the backport workflow.
const REQUIRED_TOOLS: &[&str] = &[
    "git",
    "dch",
    "uscan",
    "quilt",
    "dpkg-buildpackage",
    "lintian",
    "sbuild",
    "cargo",
    "rustup",
    "dput",
    "ppa",
];

// ── Per-phase documentation ───────────────────────────────────────────────────

/// Base URL for the official backport-rust documentation page.
const DOCS_BASE: &str = "https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/";

/// A concise explanation and documentation anchor for a single workflow phase.
///
/// Shown when the user passes `-vv` (`verbosity >= 2`).
struct PhaseDoc {
    /// One-to-three sentence plain-English explanation of what this phase does
    /// and why it exists.
    explanation: &'static str,
    /// Fragment anchor on [`DOCS_BASE`] that points to the relevant section.
    anchor: &'static str,
}

/// Per-phase documentation, indexed by phase number.
const PHASE_DOCS: &[PhaseDoc] = &[
    // Phase 0 — Preflight
    PhaseDoc {
        explanation: "Verifies that every required tool is on PATH and that the working \
            directory is the root of a Debian source package (contains debian/changelog \
            and debian/watch). Failing fast here prevents partial state from being written \
            to the repository.",
        anchor: "#backport-process",
    },
    // Phase 1 — Bug Report
    PhaseDoc {
        explanation: "Backports are either 'priority' (a downstream package such as Firefox \
            or Chromium needs this Rust version in the Ubuntu Archive) or 'proactive' \
            (pre-building the bootstrapping chain for future use). Priority backports \
            require a Launchpad bug targeting every series in the chain so that each \
            intermediate backport can be tracked independently.",
        anchor: "#launchpad-bug-report",
    },
    // Phase 2 — Git Branch
    PhaseDoc {
        explanation: "Creates a local branch '<release>-X.Y' from '<source_release>-X.Y' for \
            stable-to-stable backports. When backporting from the current devel \
            release, the '<source_release>-X.Y' branch does not exist yet — the \
            authoritative source lives on 'merge-X.Y' instead, and thermite \
            probes the Foundations remote to pick the right branch automatically. \
            Backports must go one release at a time (e.g. Noble→Jammy, never \
            Questing→Jammy directly) to isolate release-specific failures and \
            provide stable checkpoints. The branch is not pushed to the \
            Foundations repository until Phase 14, after autopkgtests pass.",
        anchor: "#setup",
    },
    // Phase 3 — Changelog
    PhaseDoc {
        explanation: "The backport version encodes the target release number twice: once in \
            the upstream component (e.g. '+dfsg~22.04') and once in the Debian revision \
            (e.g. '~22.04.1'). This ensures the backport version sorts strictly lower than \
            the same package on any newer Ubuntu release, preventing accidental upgrades \
            across series.",
        anchor: "#changelog-version",
    },
    // Phase 4 — Compatibility Checks
    PhaseDoc {
        explanation: "The target release's archive may have older versions of build \
            dependencies than the source release's packaging assumes. Six compatibility \
            checks are performed in order before tarball generation: \
            (1) LLVM version, (2) libgit2 version, (3) dh-cargo availability, \
            (4) pkgconf availability, (5) cmake version, (6) debhelper-compat level. \
            Each check infers the required version from the source packaging, queries \
            the archive via rmadison, and reports whether action is needed. Fixes are \
            applied before the orig tarball is generated so that vendored sources \
            (e.g. LLVM, libgit2) are included in a single uscan run.",
        anchor: "#common-backporting-changes",
    },
    // Phase 5 — Orig Tarball
    PhaseDoc {
        explanation: "uscan downloads and filters the upstream Rust source according to \
            'Files-Excluded' in debian/copyright. If LLVM or libgit2 vendoring is needed \
            (see the compatibility checks in Phase 4), 'Files-Excluded' must be edited first \
            and the tarball regenerated before running this phase. The tarball is renamed to include \
            '~<series>' so its filename matches the backport version string.",
        anchor: "#generating-the-orig-tarball",
    },
    // Phase 6 — Vendor Tarball
    PhaseDoc {
        explanation: "Generates the orig-vendor tarball containing filtered Cargo crate \
            dependencies. This requires a local Rust toolchain at the exact patch version \
            being packaged (installed via rustup). Only applies to Rust 1.89 and later; \
            earlier versions bundle vendored crates directly in the orig tarball.",
        anchor: "#generating-the-orig-vendor-tarball",
    },
    // Phase 7 — Disable Self-Build Test
    PhaseDoc {
        explanation: "The 'RUST_TEST_SELFBUILD=1' autopkgtest rebuilds the compiler using \
            the just-packaged toolchain. For backports — especially those that vendor LLVM \
            — this test is resource-intensive and routinely times out on the autopkgtest \
            infrastructure. The internal stage1→stage2 bootstrap that happens during the \
            regular build is sufficient validation for backports.",
        anchor: "#disabling-autopkgtest-self-build-test",
    },
    // Phase 8 — Local Build
    PhaseDoc {
        explanation: "sbuild builds the package in a clean chroot on the host architecture, \
            validating the packaging before a slow multi-architecture PPA build. \
            'quilt pop -a' is run first to ensure patches are not pre-applied — sbuild \
            applies them from scratch in the chroot and will fail if it finds them already \
            in place. Most compatibility failures surface here and can be fixed \
            iteratively.",
        anchor: "#local-build-and-bugfixing",
    },
    // Phase 9 — Build Source Package
    PhaseDoc {
        explanation: "Prepares the installable source package (.dsc + tarballs) in the \
            parent directory by resetting quilt state, cleaning prior build artifacts, \
            and running 'dpkg-buildpackage -S'. The resulting .dsc is the input for \
            Phase 10 (lintian) and all subsequent upload steps.",
        anchor: "#local-build-and-bugfixing",
    },
    // Phase 10 — Lintian
    PhaseDoc {
        explanation: "Lintian checks the source package for Debian policy compliance \
            before spending build time on a full multi-architecture PPA build. Several tags \
            are expected for versioned rustc packages (e.g. 'field-too-long \
            Vendored-Sources-Rust') and can be safely ignored; all others must be fixed \
            or overridden with a justifying comment.",
        anchor: "#lintian",
    },
    // Phase 11 — PPA Build
    PhaseDoc {
        explanation: "A personal Launchpad PPA validates all supported architectures, \
            including riscv64 which runs under emulation (expect 5–10× slower builds than \
            on native architectures). PPA Ubuntu-dependencies must be set to 'Security' \
            (not 'Proposed') because backports target the security pocket. If the bootstrap \
            compiler is only in the staging PPA, add ppa:rust-toolchain/staging as an \
            explicit PPA dependency.",
        anchor: "#ppa-build",
    },
    // Phase 12 — Staging PPA Upload
    PhaseDoc {
        explanation: "ppa:rust-toolchain/staging is the integration point for the entire \
            bootstrapping chain — every subsequent backport in the chain depends on what is \
            published here. The ~ppa<N> suffix is removed and the changelog entry must \
            enumerate every change made during the backport; a description of \
            'Backport to <release>' alone is not sufficient for reviewers.",
        anchor: "#uploading-the-backport-to-the-staging-ppa",
    },
    // Phase 13 — Autopkgtests
    PhaseDoc {
        explanation: "Triggers autopkgtest runs for all architectures against the staging \
            PPA using the 'ppa tests' command. Every test except the disabled self-build \
            test must pass on every architecture before proceeding. Do not push the branch \
            or request an archive upload until this phase is green.",
        anchor: "#autopkgtests",
    },
    // Phase 14 — Push Branch
    PhaseDoc {
        explanation: "The completed branch is pushed to the Foundations repository only \
            after autopkgtests pass. This ordering guarantees that the branch on 'origin' \
            always represents a verified, shippable state. The branch is the authoritative \
            record of what was done and must be referenced in any Archive upload request.",
        anchor: "#backport-process",
    },
    // Phase 15 — Archive Upload
    PhaseDoc {
        explanation: "Archive upload is only required for priority backports where a \
            downstream package (Firefox, Chromium, etc.) needs this Rust version in the \
            Ubuntu Archive. For proactive backports that are simply pre-building the \
            bootstrapping chain, publishing to the staging PPA is sufficient. Contact the \
            Ubuntu Security team with the bug link, staging PPA link, and package version.",
        anchor: "#uploading-the-backport-to-the-archive-optional",
    },
];

/// Print per-phase documentation when verbosity is >= 2 (`-vv`).
///
/// Called immediately after [`print_phase_header`] for every phase.
pub fn print_phase_explanation(phase: usize) {
    if crate::shell::verbosity() < 2 {
        return;
    }
    let Some(doc) = PHASE_DOCS.get(phase) else {
        return;
    };
    print_info_box(
        "About this phase",
        &[
            doc.explanation,
            "",
            &format!("Documentation: {DOCS_BASE}{}", doc.anchor),
        ],
    );
}

/// Handle the case where the self-build test stanza in `debian/tests/control`
/// does not match any shape that thermite knows how to remove automatically.
///
/// Prints the relevant documentation link and prompts the user to either
/// confirm they have manually removed and committed the stanza, or skip the
/// removal (leaving the self-build autopkgtest active, which is likely to time
/// out on the autopkgtest infrastructure).
async fn handle_manual_self_build_removal() {
    let docs_url = format!("{DOCS_BASE}#disabling-autopkgtest-self-build-test");
    print_info_box(
        "Self-build test stanza not recognised",
        &[
            "thermite found the 'RUST_TEST_SELFBUILD=1' marker in debian/tests/control \
             but the surrounding stanza does not match any known shape, so it has \
             been left untouched to avoid leaving the file in a broken state.",
            "",
            "Open debian/tests/control and delete the entire self-build test stanza \
             (the Test-Command line plus its Depends, Restrictions, comment, and \
             Architecture lines), then commit the change.",
            "",
            &format!("Documentation: {docs_url}"),
        ],
    );
    let options = [
        "I've removed and committed the stanza — continue",
        "Skip this removal (leave the self-build test active)",
    ];
    let choice = prompt_select("How would you like to proceed?", &options, 0);
    match choice {
        0 => {
            println!("  Continuing after manual self-build test removal.");
        }
        _ => {
            println!(
                "  WARNING: the self-build autopkgtest remains enabled and will \
                 likely time out on the autopkgtest infrastructure. If this \
                 backport vendors LLVM, consider disabling it before uploading."
            );
        }
    }
}

/// Pure decision logic for the source-branch resolver used in Phase 2.
///
/// Given the two candidate branch names (`primary` = `<source_release>-X.Y`,
/// `fallback` = `merge-X.Y`) and whether each exists on the remote, return the
/// branch name to check out, or `None` if neither exists (in which case the
/// caller prompts the user for a branch name interactively).
///
/// Extracted as a free function so it can be unit-tested without touching git.
fn resolve_source_branch_name<'a>(
    primary: &'a str,
    fallback: &'a str,
    has_primary: bool,
    has_fallback: bool,
) -> Option<&'a str> {
    if has_primary {
        Some(primary)
    } else if has_fallback {
        Some(fallback)
    } else {
        None
    }
}

/// Run the full `thermite backport` workflow.
pub async fn run(params: &BackportParams, repo_dir: &Path) -> Result<()> {
    let rust_ver = &params.rust_version;
    let rust_short = rust_ver.short();
    let source_release = params.source_release.as_str();
    let source_series = params.source_release.series_number();
    let release = params.release.as_str();
    let target_series = params.release.series_number();
    let lpuser = &params.lpuser;
    let git_remote = &params.git_remote;
    let mut lp_bug: Option<String> = params.lp_bug_number.clone();

    // Derived names used throughout.
    let pkg_name = format!("rustc-{rust_short}");
    let bugs_url = format!("https://bugs.launchpad.net/ubuntu/+source/{pkg_name}");
    let filebug_url = format!("{bugs_url}/+filebug");
    // Primary candidate for the source branch. When the source release is a
    // stable release, this branch exists on the Foundations remote. When the
    // source release is the current devel release, this branch does not exist
    // yet — the authoritative source lives on `merge-X.Y` instead. Phase 2
    // resolves the actual branch to use by probing the remote.
    let primary_source_branch = format!("{source_release}-{rust_short}");
    let fallback_source_branch = format!("merge-{rust_short}");
    let target_branch = format!("{release}-{rust_short}");
    let ppa_name = format!("rustc-{rust_short}-{release}");
    let parent_dir = repo_dir.parent().unwrap_or(repo_dir).to_path_buf();

    let bug_display = lp_bug
        .as_deref()
        .map(|b| format!("#{b}"))
        .unwrap_or_else(|| "(none — proactive backport)".to_owned());

    // ── Phase 0: Preflight Checks ─────────────────────────────────────────
    print_phase_header(0, "Preflight Checks");
    print_phase_explanation(0);

    // Verify required tools are on PATH.
    let tool_checks: Vec<(&str, bool)> = REQUIRED_TOOLS
        .iter()
        .map(|t| (*t, shell::which(t).is_ok()))
        .collect();
    print_tool_checks(&tool_checks);
    let missing_tools: Vec<&str> = tool_checks
        .iter()
        .filter(|(_, found)| !found)
        .map(|(name, _)| *name)
        .collect();
    if !missing_tools.is_empty() {
        eprintln!("The following required tools were not found on PATH:");
        for t in &missing_tools {
            eprintln!("  \u{2717} {t}");
        }
        return Err(ThermiteError::CommandNotFound(missing_tools.join(", ")));
    }

    // Verify the working directory is a Debian package root.
    if !repo_dir.join("debian/changelog").exists() || !repo_dir.join("debian/watch").exists() {
        return Err(ThermiteError::NotADebianPackageRoot(
            repo_dir.display().to_string(),
        ));
    }

    println!();

    print_info_box(
        "Backport Parameters",
        &[
            &format!("  Rust version     : {rust_ver} (rustc-{rust_short})"),
            &format!("  Source release   : {source_release} (series {source_series})"),
            &format!("  Target release   : {release} (series {target_series})"),
            &format!("  Launchpad user   : {lpuser}"),
            &format!("  Git remote       : {git_remote}"),
            &format!("  LP bug number    : {bug_display}"),
            &format!("  Open bugs        : {bugs_url}"),
            &format!("  Repo dir         : {}", repo_dir.display()),
        ],
    );

    // If the user passed `--source-release devel`, surface the resolved
    // concrete adjective before any further processing so that the rest of
    // the output (and the adjacency check) is unambiguous.
    if params.source_release_is_devel_alias {
        println!(
            "  Note: --source-release 'devel' resolved to '{source_release}' \
             (current Ubuntu development release)."
        );
    }

    // ── One-release-at-a-time adjacency check ─────────────────────────────
    //
    // Backports should go one release at a time along the LTS+devel chain
    // (e.g. devel→resolute→noble→jammy→focal) so that release-specific
    // failures are isolated and each step has a stable checkpoint. When the
    // source and target span more than one step on that chain, warn the user
    // and list the skipped intermediate releases before proceeding.
    let source_pos = params.source_release.chain_position();
    let target_pos = params.release.chain_position();
    let proceed_prompt = match (source_pos, target_pos) {
        (Some(sp), Some(tp)) => {
            let distance = sp.abs_diff(tp);
            if distance <= 1 {
                // Adjacent (or the same release, which params validation
                // already rejects) — no warning.
                "Proceed with these parameters?"
            } else {
                // Both in the chain but not adjacent — multi-step backport.
                let (upper, lower) = if sp > tp { (sp, tp) } else { (tp, sp) };
                let chain = crate::types::ubuntu::UbuntuRelease::backport_chain();
                let skipped: Vec<String> = chain[lower + 1..upper]
                    .iter()
                    .map(|name| {
                        let r = crate::types::ubuntu::UbuntuRelease::parse(name)
                            .expect("chain entries are valid releases");
                        let series = r.series_number();
                        let kind = if r.is_devel() { "devel" } else { "LTS" };
                        format!("  - {name} (series {series}, {kind})")
                    })
                    .collect();
                let skipped_block = skipped.join("\n");
                let source_kind = if params.source_release.is_devel() {
                    "devel"
                } else {
                    "LTS"
                };
                let target_kind = if params.release.is_devel() {
                    "devel"
                } else {
                    "LTS"
                };
                print_info_box(
                    "Multi-step backport detected",
                    &[
                        &format!(
                            "  Source : {source_release} (series {source_series}, {source_kind})"
                        ),
                        &format!("  Target : {release} (series {target_series}, {target_kind})"),
                        "",
                        "This backport spans more than one release on the LTS+devel chain:",
                        &skipped_block,
                        "",
                        "Backporting one release at a time is recommended so that \
                         release-specific failures are isolated and each step has a \
                         stable checkpoint. Backport through each skipped release \
                         in turn before attempting this longer hop.",
                    ],
                );
                "Proceed with this multi-step backport anyway?"
            }
        }
        _ => {
            // At least one release is not in the LTS+devel chain (a non-LTS,
            // non-devel release such as `oracular` or `questing`).
            let non_lts: Vec<String> = [
                (&params.source_release, "source"),
                (&params.release, "target"),
            ]
            .iter()
            .filter(|(r, _)| r.chain_position().is_none())
            .map(|(r, role)| {
                format!(
                    "  - {role}: {} (series {}, non-LTS, non-devel)",
                    r.as_str(),
                    r.series_number()
                )
            })
            .collect();
            print_info_box(
                "Non-LTS release in backport",
                &[
                    "Backports normally target Ubuntu LTS releases. The following \
                     release(s) in this backport are not LTS and not the current \
                     devel release:",
                    &non_lts.join("\n"),
                    "",
                    "The one-release-at-a-time check only applies to the LTS+devel \
                     chain and is skipped here. Proceed with caution — non-LTS \
                     releases may not have the full bootstrapping chain available.",
                ],
            );
            "Proceed with these parameters?"
        }
    };

    if prompt_select(proceed_prompt, &["Proceed", "Abort"], 0) != 0 {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 1: Create a Bug Report ─────────────────────────────────────────
    print_phase_header(1, "Create a Bug Report");
    print_phase_explanation(1);

    match lp_bug.as_deref() {
        Some(bug) => {
            print_info_box(
                "Launchpad bug",
                &[
                    &format!("LP bug #{bug} has been provided on the command line."),
                    "",
                    &format!("Check for existing open bugs against {pkg_name} first:"),
                    &format!("  {bugs_url}"),
                    "",
                    "If no suitable bug exists yet, file one at:",
                    &format!("  {filebug_url}"),
                    "",
                    "If backporting across multiple releases, target the bug to all affected series so each intermediate backport can be tracked.",
                ],
            );
            if prompt_select(
                "Confirm bug status, then continue.",
                &["Continue", "Abort"],
                0,
            ) != 0
            {
                println!("Aborted.");
                return Ok(());
            }
        }
        None => {
            print_info_box(
                "Proactive backport — bug report optional",
                &[
                    "No LP bug number was provided. This is fine for proactive backports.",
                    "",
                    &format!(
                        "First, check whether there are already open bugs against {pkg_name}:"
                    ),
                    &format!("  {bugs_url}"),
                    "",
                    "If this backport is for a specific reason (e.g. a package that needs a newer Rust to build), file a Launchpad bug:",
                    &format!("  {filebug_url}"),
                    "",
                    "All backports (with or without a bug) are uploaded to the staging PPA:",
                    "  https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/",
                ],
            );
            match prompt_select(
                "How would you like to proceed with bug tracking?",
                &[
                    "Continue without a bug report (proactive backport)",
                    "Enter a Launchpad bug number",
                    "Abort",
                ],
                0,
            ) {
                0 => { /* continue with lp_bug = None */ }
                1 => loop {
                    let input = prompt_input("LP bug number (digits only):");
                    if input.is_empty() {
                        println!("  Bug number cannot be empty. Please try again.");
                        continue;
                    }
                    if !input.chars().all(|c| c.is_ascii_digit()) {
                        println!("  Bug number must contain only digits. Please try again.");
                        continue;
                    }
                    println!(
                        "  LP bug: https://bugs.launchpad.net/ubuntu/+source/rustc/+bug/{input}"
                    );
                    lp_bug = Some(input);
                    break;
                },
                _ => {
                    println!("\n  Aborted.");
                    return Ok(());
                }
            }
        }
    }

    // ── Phase 2: Set Up Git Branch ───────────────────────────────────────────
    print_phase_header(2, "Set Up Git Branch");
    print_phase_explanation(2);

    // For backports we create the branch locally; it will be pushed to the
    // remote only once autopkgtests pass (Phase 14).
    //
    // When the target branch already exists (a previous run was aborted after
    // the branch was created), `git fetch --all` and the source-branch checkout
    // add no value — the branch is already local and we just need to switch to
    // it to resume. The fetch can be slow on poor connections, so it is
    // skipped in this case.
    if git::branch_exists(repo_dir, &target_branch).await? {
        print_info_box(
            &format!("Branch '{target_branch}' already exists"),
            &[
                "A previous run may have been interrupted after this branch was created.",
                "",
                &format!("Switching to '{target_branch}' and continuing from where it left off."),
                "Skipping 'git fetch --all' since the branch is already local.",
                &format!("To start fresh instead, exit and run: git branch -D {target_branch}"),
            ],
        );
        if prompt_select(
            &format!("Switch to '{target_branch}' and continue?"),
            &["Switch and continue", "Abort"],
            0,
        ) != 0
        {
            println!(
                "\n  Aborted. Delete the branch with \
                 'git branch -D {target_branch}' then rerun."
            );
            return Ok(());
        }
        info!("switching to existing branch {target_branch}");
        git::checkout_branch(repo_dir, &target_branch).await?;
        println!("  Switched to existing branch '{target_branch}'.");
    } else {
        // Target branch does not exist — fetch the latest state and create it
        // from the source branch.
        info!("fetching all remotes");
        git::fetch_all(repo_dir).await?;

        // Resolve the source branch to check out from. The primary candidate
        // is `<source_release>-X.Y` (used for stable-to-stable backports). When
        // the source release is the current devel release, that branch does not
        // exist yet — the authoritative source lives on `merge-X.Y` instead.
        // Probe the Foundations remote to decide which to use.
        let is_devel_source = params.source_release.is_devel();
        let has_primary =
            git::remote_branch_exists(repo_dir, git_remote, &primary_source_branch).await?;
        let has_fallback =
            git::remote_branch_exists(repo_dir, git_remote, &fallback_source_branch).await?;

        let resolved_source_branch: String = match resolve_source_branch_name(
            &primary_source_branch,
            &fallback_source_branch,
            has_primary,
            has_fallback,
        ) {
            Some(branch) => {
                if branch == fallback_source_branch {
                    // We're falling back to `merge-X.Y`. Explain why.
                    if is_devel_source {
                        print_info_box(
                            "Backporting from the current devel release",
                            &[
                                &format!(
                                    "  {source_release} is the current Ubuntu development release."
                                ),
                                &format!(
                                    "  No '{primary_source_branch}' branch exists yet on '{git_remote}';"
                                ),
                                &format!(
                                    "  using the authoritative development branch '{fallback_source_branch}' instead."
                                ),
                                "",
                                &format!(
                                    "  Once {source_release} is released, a '{primary_source_branch}' branch"
                                ),
                                "  will be created and this fallback will no longer be needed.",
                            ],
                        );
                    } else {
                        print_info_box(
                            "Source branch fallback",
                            &[
                                &format!(
                                    "  No '{primary_source_branch}' branch found on '{git_remote}'."
                                ),
                                &format!(
                                    "  Falling back to '{fallback_source_branch}' as the source branch."
                                ),
                            ],
                        );
                    }
                }
                branch.to_owned()
            }
            None => {
                // Neither candidate exists — prompt the user for the branch.
                print_info_box(
                    "Source branch not found",
                    &[
                        &format!(
                            "Neither '{primary_source_branch}' nor '{fallback_source_branch}' exists on '{git_remote}'."
                        ),
                        "",
                        "This may mean:",
                        &format!(
                            "  - the source release backport has not been pushed to '{git_remote}' yet, or"
                        ),
                        "  - the source release uses a non-standard branch naming convention.",
                        "",
                        "Enter the name of the branch to use as the backport source,",
                        "or leave blank to abort.",
                    ],
                );
                let input = prompt_input("Source branch name:");
                if input.is_empty() {
                    println!("\n  Aborted.");
                    return Ok(());
                }
                input
            }
        };

        info!("checking out {resolved_source_branch}");
        git::checkout_branch(repo_dir, &resolved_source_branch).await?;

        info!("creating branch {target_branch}");
        crate::shell::run_command("git", &["checkout", "-b", &target_branch], repo_dir, &[])
            .await?;
        println!("  Branch '{target_branch}' created from '{resolved_source_branch}'.");
    }

    // ── Phase 3: Update Changelog ────────────────────────────────────────────
    print_phase_header(3, "Update Changelog");
    print_phase_explanation(3);

    let changelog_path = repo_dir.join("debian/changelog");

    info!("reading current version from debian/changelog");
    let current_version = changelog::read_current_version(&changelog_path)?;
    println!("  Current version  : {current_version}");

    let computed_version = changelog::compute_backport_version(&current_version, target_series);
    println!("  Computed version : {computed_version}");

    if computed_version == current_version {
        println!(
            "  Note: changelog already at the computed version — selecting it will keep \
             the existing entry (no new dch entry created)."
        );
    }

    let new_version = match prompt_select(
        "How would you like to set the changelog version?",
        &[
            &format!("Use computed version ({computed_version})"),
            "Enter a custom version",
            "Abort",
        ],
        0,
    ) {
        0 => computed_version,
        1 => {
            let input = prompt_input("Enter the desired version:");
            if input.is_empty() {
                println!(concat!(
                    "\n  Aborted. Update debian/changelog manually with",
                    " 'dch -v <version>' to set the correct version,",
                    " then rerun thermite backport."
                ));
                return Ok(());
            }
            input
        }
        _ => {
            println!(concat!(
                "\n  Aborted. Update debian/changelog manually with",
                " 'dch -v <version>' to set the correct version,",
                " then rerun thermite backport."
            ));
            return Ok(());
        }
    };

    // Only create a new changelog entry when the chosen version actually
    // differs from the current top-of-changelog version.  On re-runs (where
    // Phase 3 already wrote the backport version) the computed version is
    // idempotent, so skipping dch prevents creating a duplicate entry.
    // `update_backport_changelog_entry` below still runs unconditionally to
    // fix up the distribution and bullet text on the top entry.
    if new_version != current_version {
        info!("running dch with version {new_version}");
        changelog::run_dch(repo_dir, &new_version).await?;
    } else {
        println!("  Changelog already at version {new_version} — no new entry created.");
    }

    info!("updating changelog entry distribution and description");
    changelog::update_backport_changelog_entry(&changelog_path, release, lp_bug.as_deref())?;

    let first_lines: String = std::fs::read_to_string(&changelog_path)?
        .lines()
        .take(6)
        .map(|l| format!("    {l}\n"))
        .collect();
    println!("  Changelog updated. First entry now:\n{first_lines}");

    // Commit the backport changelog entry so later phases (Phase 11) can use
    // `git restore debian/changelog` to revert only the temporary ~ppa<N>
    // entry without discarding this Phase 3 entry.  Skip the commit when the
    // working tree already matches HEAD (re-run after a prior successful run).
    let changelog_status = crate::shell::run_command(
        "git",
        &["status", "--porcelain", "debian/changelog"],
        repo_dir,
        &[],
    )
    .await?;
    if !changelog_status.stdout.trim().is_empty() {
        git::add_and_commit(
            repo_dir,
            &["debian/changelog"],
            "Add backport changelog entry",
        )
        .await?;
        println!("  Backport changelog entry committed.");
    } else {
        println!("  Backport changelog entry already committed — nothing to commit.");
    }

    // ── Phase 4: Compatibility Checks ────────────────────────────────────────
    print_phase_header(4, "Compatibility Checks");
    print_phase_explanation(4);

    info!(
        "running compatibility checks for target release {}",
        release
    );
    let check_results = compat::run_all_checks(repo_dir, release).await;

    // Track how many checks need attention so the summary line is accurate.
    let mut ok_count = 0usize;
    let mut attention_count = 0usize;
    let mut infer_failed_count = 0usize;
    let mut archive_check_failed_count = 0usize;

    for result in &check_results {
        println!("\n  {}", result.name);
        match &result.inference {
            compat::Inference::Inferred { value, source } => {
                if value.is_empty() {
                    println!("    Inferred: (no version constraint) ({source})");
                } else {
                    println!("    Inferred: {value} ({source})");
                }
            }
            compat::Inference::CouldNotInfer(reason) => {
                println!("    Could not infer automatically.");
                println!("    Reason: {reason}");
                infer_failed_count += 1;
            }
        }

        match &result.archive_status {
            compat::ArchiveStatus::Available(version) => {
                println!("    Archive: \u{2714} available ({version})");
                if result.inference.is_inferred() {
                    ok_count += 1;
                }
            }
            compat::ArchiveStatus::TooOld {
                available,
                required,
            } => {
                println!(
                    "    Archive: \u{2717} too old — available {available}, required {required}"
                );
                attention_count += 1;
            }
            compat::ArchiveStatus::NotPublished => {
                println!("    Archive: \u{2717} not published in {release}");
                attention_count += 1;
            }
            compat::ArchiveStatus::CheckFailed(detail) => {
                println!("    Archive: could not check ({detail})");
                archive_check_failed_count += 1;
            }
        }

        if !result.is_ok() {
            if !result.guidance.is_empty() {
                println!("    Action needed: {}", result.guidance);
            }
            println!("    Reference: {}", result.url);
        }
    }

    // Print a summary line.
    println!();
    let total = check_results.len();
    if attention_count == 0 && infer_failed_count == 0 && archive_check_failed_count == 0 {
        println!("  Summary: {ok_count}/{total} checks passed — no action needed.");
    } else {
        let parts: Vec<String> = [
            if attention_count > 0 {
                format!("{attention_count} need attention")
            } else {
                String::new()
            },
            if infer_failed_count > 0 {
                format!("{infer_failed_count} could not infer")
            } else {
                String::new()
            },
            if archive_check_failed_count > 0 {
                format!("{archive_check_failed_count} archive check failed")
            } else {
                String::new()
            },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
        println!(
            "  Summary: {ok_count}/{total} checks passed, {}.",
            parts.join(", ")
        );

        print_info_box(
            "Guidance for checks needing attention",
            &[
                "Apply all needed changes to debian/control, debian/control.in,",
                "debian/copyright, debian/rules, and debian/config.toml.in as",
                "described above. Commit them together before continuing.",
                "",
                "If LLVM or libgit2 vendoring is needed, the orig tarball (Phase 5)",
                "must be regenerated after editing Files-Excluded in debian/copyright.",
                "",
                "Full documentation:",
                "  https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/",
            ],
        );
    }

    if prompt_select(
        "All applicable compatibility changes worked through and committed?",
        &["Continue", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 5: Generate Orig Tarball ───────────────────────────────────────
    print_phase_header(5, "Generate Orig Tarball");
    print_phase_explanation(5);

    // The expected final tarball name encodes the target series suffix so that
    // the filename matches the backport version string (e.g. ~20.04).
    let expected_tarball_name =
        format!("rustc-{rust_short}_{rust_ver}+dfsg~{target_series}.orig.tar.xz");
    let expected_tarball = parent_dir.join(&expected_tarball_name);

    let reuse_header = if expected_tarball.exists() {
        "  REUSE      — No Files-Excluded change; tarball already exists at:"
    } else {
        "  REUSE      — No Files-Excluded change; tarball NOT detected at the path below — Reuse will fail until it exists:"
    };

    print_info_box(
        "Tarball decision",
        &[
            "Choose how to provide the orig tarball for this backport:",
            "",
            reuse_header,
            &format!("               {}", expected_tarball.display()),
            "",
            "  DOWNLOAD   — No Files-Excluded change; tarball not yet local. Fetch automatically from the staging PPA or Ubuntu archive (falls back to manual placement, file named exactly):",
            &format!("               {expected_tarball_name}"),
            &format!("               and place it in: {}", parent_dir.display()),
            "",
            "  REGENERATE — Files-Excluded in debian/copyright was changed (e.g. LLVM or libgit2 vendoring — see Phase 4). uscan will run now; takes 20–60 minutes.",
        ],
    );

    let reuse_label = if expected_tarball.exists() {
        "Reuse      — tarball already exists in the parent directory"
    } else {
        "Reuse      — tarball NOT detected in the parent directory — will fail unless you place it there first"
    };

    let tarball = match prompt_select(
        "How would you like to provide the orig tarball?",
        &[
            reuse_label,
            "Download   — fetch automatically from the staging PPA / Ubuntu archive (falls back to manual placement)",
            "Regenerate — run uscan now (20–60 min; required if Files-Excluded changed)",
            "Abort",
        ],
        0,
    ) {
        0 => {
            // Reuse: verify the tarball is present locally.
            if !expected_tarball.exists() {
                return Err(crate::error::ThermiteError::CommandFailed {
                    cmd: "reuse orig tarball".to_owned(),
                    code: 0,
                    stdout: String::new(),
                    stderr: format!(
                        "tarball not found: {}\n\
                         Select 'Download' or 'Regenerate' to obtain it first.",
                        expected_tarball.display(),
                    ),
                });
            }
            expected_tarball
        }
        1 => {
            // Download: try to fetch automatically from the staging PPA or the
            // Ubuntu archive; fall back to manual placement.
            let fetched = tarball_fetch::fetch_tarball(
                &rust_short,
                rust_ver,
                &format!("~{target_series}"),
                &parent_dir,
                tarball_fetch::TarballKind::Orig,
            )
            .await?;
            if fetched.is_none() {
                println!("  Automated download unavailable — place the tarball manually.");
                if prompt_select(
                    "Place the tarball at the path shown above, then continue.",
                    &["I've placed the tarball — continue", "Abort"],
                    0,
                ) != 0
                {
                    println!("Aborted.");
                    return Ok(());
                }
            }
            if !expected_tarball.exists() {
                return Err(crate::error::ThermiteError::CommandFailed {
                    cmd: "download orig tarball".to_owned(),
                    code: 0,
                    stdout: String::new(),
                    stderr: format!(
                        "tarball not found: {}\n\
                         Ensure the file is named exactly '{}' and placed in '{}'.",
                        expected_tarball.display(),
                        expected_tarball_name,
                        parent_dir.display(),
                    ),
                });
            }
            expected_tarball
        }
        2 => {
            // Regenerate: run uscan with the series suffix baked into the name.
            let uscan_log = parent_dir.join(format!("uscan-{rust_ver}-backport.log"));
            info!("running uscan --download-version {rust_ver}");
            uscan::run_uscan(repo_dir, rust_ver, &format!("~{target_series}"), &uscan_log).await?
        }
        _ => {
            // Abort
            println!("\n  Aborted at Phase 5. Re-run when ready to provide the orig tarball.");
            return Ok(());
        }
    };
    println!("  Orig tarball: {}", tarball.display());

    // ── Phase 6: Generate Orig-Vendor Tarball ────────────────────────────────
    print_phase_header(6, "Generate Orig-Vendor Tarball");
    print_phase_explanation(6);

    // The vendor tarball name encodes the same series suffix as the orig
    // tarball so it matches the backport changelog version.
    let vendor_tarball_name =
        format!("rustc-{rust_short}_{rust_ver}+dfsg~{target_series}.orig-vendor.tar.xz");
    let expected_vendor_tarball = parent_dir.join(&vendor_tarball_name);

    let vendor_reuse_header = if expected_vendor_tarball.exists() {
        "  REUSE      — vendor tarball already exists at:"
    } else {
        "  REUSE      — vendor tarball NOT detected at the path below — Reuse will fail until it exists:"
    };

    print_info_box(
        "Vendor tarball decision",
        &[
            "Choose how to provide the orig-vendor tarball for this backport:",
            "",
            vendor_reuse_header,
            &format!("               {}", expected_vendor_tarball.display()),
            "",
            "  DOWNLOAD   — Vendor tarball not yet local. Fetch automatically from the staging PPA or Ubuntu archive (e.g. from a previous build attempt; falls back to manual placement, file named exactly):",
            &format!("               {vendor_tarball_name}"),
            &format!("               and place it in: {}", parent_dir.display()),
            "",
            "  REGENERATE — Files-Excluded in debian/copyright was changed (e.g. LLVM or libgit2 vendoring — see Phase 4). Installs the matching Rust toolchain via rustup, then runs `debian/rules vendor-tarball`; takes several minutes.",
        ],
    );

    let vendor_reuse_label = if expected_vendor_tarball.exists() {
        "Reuse      — vendor tarball already exists in the parent directory"
    } else {
        "Reuse      — vendor tarball NOT detected in the parent directory — will fail unless you place it there first"
    };

    let vendor_tarball = match prompt_select(
        "How would you like to provide the orig-vendor tarball?",
        &[
            vendor_reuse_label,
            "Download   — fetch automatically from the staging PPA / Ubuntu archive (falls back to manual placement)",
            "Regenerate — build it now (installs the Rust toolchain via rustup; slow)",
            "Abort",
        ],
        0,
    ) {
        0 => {
            // Reuse: verify the vendor tarball is present locally.
            if !expected_vendor_tarball.exists() {
                return Err(crate::error::ThermiteError::CommandFailed {
                    cmd: "reuse orig-vendor tarball".to_owned(),
                    code: 0,
                    stdout: String::new(),
                    stderr: format!(
                        "vendor tarball not found: {}\n\
                         Select 'Download' or 'Regenerate' to obtain it first.",
                        expected_vendor_tarball.display(),
                    ),
                });
            }
            expected_vendor_tarball
        }
        1 => {
            // Download: try to fetch automatically from the staging PPA or the
            // Ubuntu archive; fall back to manual placement.
            let fetched = tarball_fetch::fetch_tarball(
                &rust_short,
                rust_ver,
                &format!("~{target_series}"),
                &parent_dir,
                tarball_fetch::TarballKind::OrigVendor,
            )
            .await?;
            if fetched.is_none() {
                println!("  Automated download unavailable — place the vendor tarball manually.");
                if prompt_select(
                    "Place the vendor tarball at the path shown above, then continue.",
                    &["I've placed the vendor tarball — continue", "Abort"],
                    0,
                ) != 0
                {
                    println!("Aborted.");
                    return Ok(());
                }
            }
            if !expected_vendor_tarball.exists() {
                return Err(crate::error::ThermiteError::CommandFailed {
                    cmd: "download orig-vendor tarball".to_owned(),
                    code: 0,
                    stdout: String::new(),
                    stderr: format!(
                        "vendor tarball not found: {}\n\
                         Ensure the file is named exactly '{}' and placed in '{}'.",
                        expected_vendor_tarball.display(),
                        vendor_tarball_name,
                        parent_dir.display(),
                    ),
                });
            }
            expected_vendor_tarball
        }
        2 => {
            // Regenerate: install the Rust toolchain (deferred until needed),
            // prune any stale series tarballs, and rebuild from scratch.
            if expected_vendor_tarball.exists() {
                std::fs::remove_file(&expected_vendor_tarball)
                    .map_err(crate::error::ThermiteError::Io)?;
            }
            info!("installing Rust toolchain {rust_ver}");
            let rust_bootstrap_dir = vendor::rustup_install_toolchain(rust_ver).await?;
            vendor::generate_vendor_tarball_clean(
                repo_dir,
                &rust_bootstrap_dir,
                rust_ver,
                &format!("~{target_series}"),
            )
            .await?
        }
        _ => {
            // Abort
            println!(
                "\n  Aborted at Phase 6. Re-run when ready to provide the orig-vendor tarball."
            );
            return Ok(());
        }
    };
    println!("  Vendor tarball: {}", vendor_tarball.display());

    // ── Phase 7: Disable Autopkgtest Self-Build Test ─────────────────────────
    // H2 fix: this phase is now before the local build.
    print_phase_header(7, "Disable Autopkgtest Self-Build Test");
    print_phase_explanation(7);

    info!("removing self-build test from debian/tests/control");
    let outcome = build::disable_self_build_test(repo_dir)?;

    match outcome {
        build::SelfBuildTestOutcome::Removed => {
            let status_output = crate::shell::run_command(
                "git",
                &["status", "--porcelain", "debian/tests/control"],
                repo_dir,
                &[],
            )
            .await?;
            if !status_output.stdout.trim().is_empty() {
                git::add_and_commit(
                    repo_dir,
                    &["debian/tests/control"],
                    "Disable autopkgtest self-build test for backport",
                )
                .await?;
                println!("  Self-build test block removed and committed.");
            } else {
                println!("  Self-build test block was already absent — nothing to commit.");
            }
        }
        build::SelfBuildTestOutcome::AlreadyAbsent => {
            println!("  Self-build test block was already absent — nothing to commit.");
        }
        build::SelfBuildTestOutcome::NeedsManualIntervention => {
            handle_manual_self_build_removal().await;
        }
    }

    // ── Phase 8: Local Build and Bug Fixing ──────────────────────────────────
    print_phase_header(8, "Local Build and Bug Fixing");
    print_phase_explanation(8);

    print_info_box(
        "About to run sbuild",
        &[
            "The local build may fail if the target Ubuntu release has older versions of certain dependencies. Use the Phase 4 compatibility guidance to diagnose failures.",
            "",
            "Consult the backporting guide for detailed diagnostics:",
            "  https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/",
            "",
            "sbuild will use ppa:rust-toolchain/staging as an extra repository so the bootstrap compiler is available.",
        ],
    );
    match prompt_select(
        "Ready to start the local build?",
        &["Start build", "Skip — already built locally", "Abort"],
        0,
    ) {
        0 => {
            if !run_interactive_local_build(repo_dir, &parent_dir, release).await? {
                println!("Aborted.");
                return Ok(());
            }
        }
        1 => {
            println!("  Local build skipped.");
        }
        _ => {
            println!("Aborted.");
            return Ok(());
        }
    }

    // ── Phase 9: Build Source Package ────────────────────────────────────────
    print_phase_header(9, "Build Source Package");
    print_phase_explanation(9);

    // The expected .dsc path is deterministic from the package name and version.
    let dsc_path = parent_dir.join(format!("{pkg_name}_{new_version}.dsc"));

    let build_source = match prompt_select(
        "Build the source package now?",
        &[
            "Build source package",
            "Skip — .dsc already exists in parent directory",
            "Abort",
        ],
        0,
    ) {
        0 => true,
        1 => {
            if dsc_path.exists() {
                println!("  Found: {}", dsc_path.display());
                false
            } else {
                print_info_box(
                    "Source package not found",
                    &[
                        &format!("Expected: {}", dsc_path.display()),
                        "",
                        "Phase 10 (lintian) requires this file. If you also intend to skip lintian, you may continue without it.",
                    ],
                );
                match prompt_select(
                    "How would you like to proceed?",
                    &[
                        "Build source package now",
                        "Continue without it (only safe if also skipping Phase 10)",
                        "Abort",
                    ],
                    0,
                ) {
                    0 => true,
                    1 => false,
                    _ => {
                        println!("Aborted.");
                        return Ok(());
                    }
                }
            }
        }
        _ => {
            println!("Aborted.");
            return Ok(());
        }
    };

    if build_source {
        info!("cleaning build artifacts before source package build");
        build::quilt_pop_all(repo_dir).await?;
        build::clean_build_artifacts(&parent_dir, repo_dir).await?;

        info!("building source package");
        build::run_dpkg_buildpackage_source(repo_dir).await?;
        println!("  Source package: {}", dsc_path.display());
    }

    // ── Phase 10: Lintian Checks ──────────────────────────────────────────────
    print_phase_header(10, "Lintian Checks");
    print_phase_explanation(10);

    'lintian: {
        match prompt_select(
            "Run lintian checks for this backport?",
            &["Run lintian checks", "Skip lintian", "Abort"],
            0,
        ) {
            1 => {
                println!("  Lintian skipped.");
                break 'lintian;
            }
            2 => {
                println!("Aborted.");
                return Ok(());
            }
            _ => {}
        }

        let lintian_log = parent_dir.join(format!("lintian-{rust_short}-{release}.log"));
        info!("running lintian");
        let lintian_output =
            lintian::run_lintian(repo_dir, &["-i", "--tag-display-limit", "0"], &lintian_log)
                .await?;

        println!("  Lintian log: {}", lintian_log.display());
        println!(
            "  Errors: {}  Warnings: {}",
            lintian_output.errors.len(),
            lintian_output.warnings.len()
        );

        print_info_box(
            "Expected (ignorable) Lintian tags for rustc backports",
            &[
                "The following tags are expected and can be ignored:",
                "",
                "  E: field-too-long Vendored-Sources-Rust",
                "     (field length is unavoidable; upstream dh-cargo fix needed)",
                "  E: unknown-file-in-debian-source [lintian-overrides.in]",
                "     (intentional — generates per-version overrides)",
                "  E: version-substvar-for-external-package Depends ${binary:Version}",
                "     (deliberate fallback, not an error)",
                "  W: unknown-field Vendored-Sources-Rust",
                "     (custom field, not a typo)",
                "  Various warnings in src/llvm-project/ (only when LLVM is vendored)",
                "     (test-suite binaries in upstream LLVM source)",
                "",
                "All other errors and warnings must be fixed or overridden with a justifying comment in debian/source/lintian-overrides{,.in}.",
            ],
        );

        if !lintian_output.errors.is_empty() || !lintian_output.warnings.is_empty() {
            if prompt_select(
                "Review lintian output, then continue.",
                &["Issues fixed or overridden — continue", "Abort"],
                0,
            ) != 0
            {
                println!("Aborted.");
                return Ok(());
            }
        } else {
            println!("  Lintian clean.");
        }
    } // end 'lintian

    // ── Phase 11: PPA Build ──────────────────────────────────────────────────
    print_phase_header(11, "PPA Build");
    print_phase_explanation(11);

    print_info_box(
        "Personal PPA build",
        &[
            "Before uploading to the staging PPA, build in a personal PPA first to confirm the package builds cleanly in the Launchpad build environment.",
            "",
            &format!("Suggested PPA name: {ppa_name}"),
            "",
            "After PPA creation, configure it:",
            "  1. Change Details → Processors: enable ALL architectures (incl. riscv64).",
            "  2. Edit PPA Dependencies → Ubuntu dependencies: set to 'Security'.",
            "     (Backports target the security pocket, not proposed.)",
            "  3. If bootstrapping from staging PPA, add ppa:rust-toolchain/staging as an explicit PPA dependency.",
        ],
    );

    if confirm_sink(
        params.dry_run,
        "Create Launchpad PPA",
        &[&format!(" PPA: {lpuser}/{ppa_name}")],
    ) {
        let ppa_url = ppa::create_ppa(&ppa_name).await?;
        if !ppa_url.is_empty() {
            println!("  PPA created: {ppa_url}");
        }
    }

    // M5: prompt for ~ppa<N> number (default 1) instead of hardcoding.
    let ppa_n_str = prompt_input("PPA upload number? [1]");
    let ppa_n: u32 = ppa_n_str.parse().unwrap_or(1);
    println!("  Using ~ppa{ppa_n} suffix.");

    // M3: quilt pop -a before dpkg-buildpackage -S.
    info!("cleaning build artifacts before PPA source build");
    build::quilt_pop_all(repo_dir).await?;
    build::clean_build_artifacts(&parent_dir, repo_dir).await?;

    ppa::add_ppa_changelog_entry(repo_dir, &new_version, release, ppa_n).await?;
    build::run_dpkg_buildpackage_source(repo_dir).await?;

    let changes_file = find_changes_file(&parent_dir)?;
    let ppa_ref = format!("{lpuser}/{ppa_name}");
    let changes_name = changes_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if confirm_sink(
        params.dry_run,
        "Upload source package to personal PPA",
        &[
            &format!("  PPA    : {ppa_ref}"),
            &format!("  Package: {changes_name}"),
        ],
    ) {
        ppa::dput_to_ppa(&ppa_ref, &changes_file).await?;
    }

    // Revert the temporary ~ppa<N> changelog entry.
    git::restore_file(repo_dir, &changelog_path).await?;
    println!("  Temporary PPA changelog entry reverted.");

    print_info_box(
        "Monitor your personal PPA build",
        &[
            &format!("  https://launchpad.net/~{lpuser}/+archive/ubuntu/{ppa_name}/+builds"),
            "",
            "Wait for the build to succeed on all architectures before proceeding.",
            "",
            "If the riscv64 build fails with unrecognised RISC-V ISA extensions (only when LLVM is vendored), cherry-pick commits for the zicsr/zmmul extensions are listed in the official backport-rust guide.",
            "",
            "If a build fails with 'No space left on device' (LLVM or libgit2 vendoring), disk-space reduction steps are described in the official backport-rust guide.",
        ],
    );
    if prompt_select(
        "Confirm personal PPA build status.",
        &["Build complete on all architectures — continue", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 12: Staging PPA Upload ─────────────────────────────────────────
    print_phase_header(12, "Staging PPA Upload");
    print_phase_explanation(12);

    print_info_box(
        "Prepare the final changelog entry",
        &[
            "Edit the top changelog entry to:",
            "  1. Remove the ~ppa<N> suffix from the version string (already done by the git restore above — verify the version looks correct).",
            "  2. Replace the placeholder description with a complete list of every change made during this backport, for example:",
            "",
            "       * Backport Rust X.Y to <release>",
            "         - Replace system LLVM dependencies with vendored version",
            "         - Downgrade libgit2 to <version>",
            "         - Replace pkgconf with pkg-config",
            "",
            "An editor will open via 'dch -r'. Save and close to proceed.",
            "After saving, the source package will be built and uploaded to:",
            "  ppa:rust-toolchain/staging",
        ],
    );
    if prompt_select(
        "Ready to open the changelog editor?",
        &["Open changelog editor", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // Open dch -r with the user's configured editor (TTY-inherited).
    crate::shell::run_interactive_command("dch", &["-r", "--no-auto-nmu"], repo_dir, &[]).await?;

    // M6: clean before staging dpkg-buildpackage -S.
    info!("cleaning build artifacts before staging source build");
    build::quilt_pop_all(repo_dir).await?;
    build::clean_build_artifacts(&parent_dir, repo_dir).await?;

    build::run_dpkg_buildpackage_source(repo_dir).await?;

    let staging_changes = find_changes_file(&parent_dir)?;
    let staging_name = staging_changes
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if confirm_sink(
        params.dry_run,
        "Upload to SHARED TEAM staging PPA",
        &[
            "  PPA    : rust-toolchain/staging",
            "  This is a shared PPA \u{2014} uploads affect the entire bootstrapping chain.",
            &format!("  Package: {staging_name}"),
        ],
    ) {
        ppa::dput_to_ppa("rust-toolchain/staging", &staging_changes).await?;
        println!("  Uploaded to ppa:rust-toolchain/staging.");
    }

    print_info_box(
        "Monitor staging PPA build",
        &[
            "  https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/+builds",
            "",
            "Wait for the build to succeed on all architectures before running autopkgtests.",
        ],
    );
    if prompt_select(
        "Confirm staging PPA build status.",
        &["Build complete on all architectures — continue", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 13: Autopkgtests ────────────────────────────────────────────────
    print_phase_header(13, "Autopkgtests");
    print_phase_explanation(13);

    let test_urls = ppa::get_staging_ppa_test_urls(&pkg_name, release).await?;
    if test_urls.is_empty() {
        print_info_box(
            "Trigger autopkgtests manually",
            &[
                "No URLs were returned by 'ppa tests'. Trigger tests manually:",
                &format!(
                    "  ppa tests ppa:rust-toolchain/staging -p {pkg_name} --release {release} --show-url"
                ),
            ],
        );
    } else {
        let mut lines = vec![
            "Click each URL to trigger an autopkgtest run for that architecture.".to_owned(),
            "Re-run the command to check status after a few minutes.".to_owned(),
            String::new(),
        ];
        lines.extend(test_urls.iter().cloned());
        lines.push(String::new());
        lines.push("Note: the self-build test has been disabled for this backport.".to_owned());
        lines.push("All other tests must pass before requesting archive upload.".to_owned());
        let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        print_info_box("Staging PPA autopkgtest URLs", &line_refs);
    }
    if prompt_select(
        "Confirm autopkgtest results.",
        &["All autopkgtests passed — continue", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 14: Push Branch to Foundations Repository ──────────────────────
    // H3 fix: branch is pushed only after autopkgtests pass.
    print_phase_header(14, "Push Branch to Foundations Repository");
    print_phase_explanation(14);

    info!("pushing {target_branch} to {git_remote}");
    if confirm_sink(
        params.dry_run,
        "Push branch to Foundations repository",
        &[
            &format!("  Branch : {target_branch}"),
            &format!("  Remote : {git_remote}"),
        ],
    ) {
        git::push_branch(repo_dir, git_remote, &target_branch).await?;
        println!("  Branch '{target_branch}' pushed to '{git_remote}'.");
        print_info_box(
            "Branch pushed",
            &[
                &format!("  Branch : {target_branch}"),
                &format!("  Remote : {git_remote}"),
                "",
                "The branch is now the authoritative record of this backport and must be referenced in any Archive upload request.",
            ],
        );
    }

    // ── Phase 15: Archive Upload (optional) ──────────────────────────────────
    print_phase_header(15, "Archive Upload (optional)");
    print_phase_explanation(15);

    print_info_box(
        "Requesting archive upload",
        &[
            "Archive upload is only needed if the backport is specifically required in the Ubuntu Archive. For bootstrapping future Rust versions, the staging PPA is sufficient.",
            "",
            "If archive upload is needed, contact the Ubuntu Security team with:",
            &format!(
                "  • Bug link      : https://bugs.launchpad.net/ubuntu/+bug/{}",
                lp_bug.as_deref().unwrap_or("<no bug>")
            ),
            "  • Staging PPA   : https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/",
            &format!("  • Package       : {pkg_name} ({new_version})"),
            "",
            "Monitor upload progress:",
            "  https://launchpad.net/~ubuntu-security-proposed/+archive/ubuntu/ppa/+packages",
        ],
    );
    prompt_select("Backport workflow complete.", &["Finish"], 0);

    println!("\nthermite backport complete.");
    Ok(())
}

// ── interactive helper loops ──────────────────────────────────────────────────

/// Build the `--extra-repository` argument for `ppa:rust-toolchain/staging`.
///
/// The staging PPA provides the bootstrap compiler (`rustc-X.Y_old`,
/// `cargo-X.Y_old`) when it has not yet landed in the target release's
/// archive.  It is always passed to sbuild during a backport build so the
/// resolver can find the bootstrap compiler without manual intervention.
///
/// Format matches the official backport-rust documentation.
fn staging_ppa_extra_repository(release: &str) -> String {
    format!(
        "--extra-repository=deb [trusted=yes] http://ppa.launchpad.net/rust-toolchain/staging/ubuntu/ {release} main"
    )
}

/// Interactive local build loop using sbuild.
///
/// Returns `Ok(true)` when the build succeeds, `Ok(false)` when the user
/// chooses to abort from the retry prompt.
async fn run_interactive_local_build(
    repo_dir: &Path,
    parent_dir: &Path,
    release: &str,
) -> Result<bool> {
    // Always include the staging PPA so sbuild can resolve the bootstrap
    // compiler (rustc-X.Y_old / cargo-X.Y_old) when it is not yet in the
    // target release's archive.
    let extra_args = vec![staging_ppa_extra_repository(release)];
    loop {
        // M3: quilt pop -a before cleaning to avoid leaving modified source
        // files without quilt tracking them.
        build::quilt_pop_all(repo_dir).await?;
        build::clean_build_artifacts(parent_dir, repo_dir).await?;
        match build::run_sbuild(repo_dir, release, &extra_args).await? {
            build::SbuildResult::Success => {
                println!("  sbuild succeeded.");
                return Ok(true);
            }
            build::SbuildResult::Failure {
                log_path,
                stdout,
                stderr,
            } => {
                let failures = log_path
                    .as_ref()
                    .and_then(|p| build::extract_test_failures(p).ok())
                    .unwrap_or_default();

                // Resolve an absolute path for display, and pick the right
                // message depending on whether sbuild produced a real build
                // log or we fell back to capturing its output.
                let log_line = match log_path
                    .as_ref()
                    .and_then(|p| std::fs::canonicalize(p).ok())
                {
                    Some(abs) => format!("Build log: {}", abs.display()),
                    None => {
                        // No file at all (even our fallback write failed).
                        // Surface a snippet of the captured output inline.
                        let snippet = if !stderr.trim().is_empty() {
                            stderr.trim()
                        } else {
                            stdout.trim()
                        };
                        let snippet: String = snippet.chars().take(400).collect();
                        format!(
                            "sbuild failed before producing a build log. Captured output:\n{snippet}"
                        )
                    }
                };

                print_info_box(
                    "Build failed — common backporting fixes",
                    &[
                        &log_line,
                        "",
                        "Consult the backporting guide for common fixes:",
                        "  https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/",
                        "",
                        "Quick reference (see Phase 4 compatibility guidance for details):",
                        "  LLVM too old       → vendor LLVM from src/llvm-project",
                        "  libgit2 too old    → downgrade or vendor libgit2",
                        "  dh-cargo missing   → comment out from Build-Depends",
                        "  pkgconf missing    → replace with pkg-config",
                        "  cmake too old      → add cmake-mozilla fallback",
                        "  debhelper-compat   → downgrade compat level",
                        "",
                        "For rustdoc-ui test failures (make < 4.4 jobserver warnings), proceed to PPA build — Launchpad builders do not trigger them.",
                        "",
                        "The staging PPA is already included via --extra-repository. If the bootstrap compiler is still not found, verify that rustc-X.Y exists in ppa:rust-toolchain/staging for this release.",
                    ],
                );
                if !failures.is_empty() {
                    println!("  Extracted {} test failure section(s).", failures.len());
                }
                match prompt_select(
                    "Build failed. What would you like to do?",
                    &[
                        "Fix failure and retry",
                        "Skip — proceed despite failure",
                        "Abort",
                    ],
                    0,
                ) {
                    0 => { /* retry — loop continues */ }
                    1 => {
                        println!(
                            "  Warning: skipping failed local build. \
                             Proceeding to source package build."
                        );
                        return Ok(true);
                    }
                    _ => return Ok(false),
                }
            }
        }
    }
}

// ── small helpers ─────────────────────────────────────────────────────────────

/// Find the newest `.changes` file in `parent_dir`.
fn find_changes_file(parent_dir: &Path) -> Result<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(parent_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".changes"))
        .collect();

    // Sort newest-first by modification time.
    entries.sort_by(|a, b| {
        let mt_a = a.metadata().and_then(|m| m.modified()).ok();
        let mt_b = b.metadata().and_then(|m| m.modified()).ok();
        mt_b.cmp(&mt_a)
    });

    entries.into_iter().next().map(|e| e.path()).ok_or_else(|| {
        crate::error::ThermiteError::CommandFailed {
            cmd: "dpkg-buildpackage".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: "no .changes file found in parent directory".to_owned(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All workflow-critical tools must appear in REQUIRED_TOOLS.
    #[test]
    fn required_tools_contains_workflow_dependencies() {
        for tool in &[
            "git",
            "dch",
            "uscan",
            "quilt",
            "dpkg-buildpackage",
            "lintian",
            "sbuild",
            "dput",
            "ppa",
        ] {
            assert!(
                REQUIRED_TOOLS.contains(tool),
                "REQUIRED_TOOLS is missing '{tool}'"
            );
        }
    }

    #[test]
    fn find_changes_file_returns_newest() {
        use std::fs;
        use std::time::Duration;

        let tmp = std::env::temp_dir().join(format!(
            "thermite-backport-changes-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        let old_path = tmp.join("rustc-1.84_amd64.changes");
        let new_path = tmp.join("rustc-1.85_amd64.changes");
        fs::write(&old_path, "old").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&new_path, "new").unwrap();

        let result = find_changes_file(&tmp).unwrap();
        assert_eq!(
            result.file_name().unwrap(),
            new_path.file_name().unwrap(),
            "should pick the newest .changes file"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn staging_ppa_extra_repository_format() {
        let arg = staging_ppa_extra_repository("noble");
        assert_eq!(
            arg,
            "--extra-repository=deb [trusted=yes] http://ppa.launchpad.net/rust-toolchain/staging/ubuntu/ noble main"
        );
    }

    #[test]
    fn staging_ppa_extra_repository_includes_release_name() {
        let arg = staging_ppa_extra_repository("jammy");
        assert!(
            arg.contains("jammy main"),
            "expected release name in extra-repository arg, got: {arg}"
        );
    }

    #[test]
    fn resolve_source_branch_name_prefers_primary_when_present() {
        assert_eq!(
            resolve_source_branch_name("resolute-1.85", "merge-1.85", true, true),
            Some("resolute-1.85")
        );
    }

    #[test]
    fn resolve_source_branch_name_falls_back_to_merge_when_primary_absent() {
        assert_eq!(
            resolve_source_branch_name("stonking-1.85", "merge-1.85", false, true),
            Some("merge-1.85")
        );
    }

    #[test]
    fn resolve_source_branch_name_returns_none_when_both_absent() {
        assert_eq!(
            resolve_source_branch_name("stonking-1.85", "merge-1.85", false, false),
            None
        );
    }

    #[test]
    fn resolve_source_branch_name_returns_none_when_only_primary_absent_and_no_fallback() {
        // primary absent, fallback absent → None (covered above, but assert the
        // asymmetric case explicitly for clarity).
        assert_eq!(
            resolve_source_branch_name("resolute-1.85", "merge-1.85", false, false),
            None
        );
    }
}
