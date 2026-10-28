use std::path::{Path, PathBuf};

use tracing::info;

use crate::error::Result;
use crate::steps::{
    autopkgtest, build, changelog, control, copyright, gbp, git, lintian, patches, ppa, uscan,
    vendor,
};
use crate::types::params::UpdateParams;
use crate::types::versions::RustVersion;
use crate::ui::{print_info_box, print_phase_header, prompt_input, prompt_select};

/// Required external tools for the update workflow.
/// Finding 9: rustup, ppa, autopkgtest, and autopkgtest-buildvm-ubuntu-cloud are
/// needed by phases 8, 19 and autopkgtest qemu runs respectively.
const REQUIRED_TOOLS: &[&str] = &[
    "git",
    "dch",
    "uscan",
    "gbp",
    "quilt",
    "dpkg-buildpackage",
    "sbuild",
    "lintian",
    "cargo",
    "rustup",
    "dput",
    "ppa",
    "autopkgtest",
    "autopkgtest-buildvm-ubuntu-cloud",
];

/// Run the full `thermite update` workflow.
pub async fn run(params: &UpdateParams, repo_dir: &Path) -> Result<()> {
    let new_ver = &params.rust_update_version;
    let old_ver = &params.rust_old_version;
    let new_short = new_ver.short();
    let old_short = old_ver.short();
    let release = params.release.as_str();
    let lpuser = &params.lpuser;
    let git_remote = &params.git_remote;
    let lp_bug = &params.lp_bug_number;

    // Derived names used throughout.
    let new_pkg = format!("rustc-{new_short}");
    let old_pkg = format!("rustc-{old_short}");
    let merge_branch = format!("merge-{new_short}");
    let old_merge_branch = format!("merge-{old_short}");
    let import_old_branch = format!("import-old-{new_short}");
    let import_new_branch = format!("import-new-{new_short}");
    let ppa_name = format!("rustc-{new_short}-merge");
    let parent_dir = repo_dir.parent().unwrap_or(repo_dir).to_path_buf();

    // ── Phase 0: Preflight Checks ────────────────────────────────────────────
    print_phase_header(0, "Preflight Checks");

    info!("checking required tools");
    for tool in REQUIRED_TOOLS {
        crate::shell::which(tool)?;
        println!("  ✓ {tool}");
    }

    git::verify_debian_package_root(repo_dir).await?;
    println!("  ✓ debian/changelog and debian/watch present");

    print_info_box(
        "Update Parameters",
        &[
            &format!("  New Rust version : {new_ver} (rustc-{new_short})"),
            &format!("  Old Rust version : {old_ver} (rustc-{old_short})"),
            &format!("  Ubuntu release   : {release}"),
            &format!("  Launchpad user   : {lpuser}"),
            &format!("  Git remote       : {git_remote}"),
            &format!("  LP bug number    : #{lp_bug}"),
            &format!("  Repo dir         : {}", repo_dir.display()),
        ],
    );
    if prompt_select("Proceed with these parameters?", &["Proceed", "Abort"], 0) != 0 {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 1: Bug Report ──────────────────────────────────────────────────
    print_phase_header(1, "Create a Bug Report");
    print_info_box(
        "Action required: create a Launchpad bug",
        &[
            "Before continuing, create a Launchpad bug for this update.",
            "",
            "For the default devel release: create a bug under rust-defaults.",
            "  https://bugs.launchpad.net/ubuntu/+source/rust-defaults/+filebug",
            "",
            "For non-default releases: create a general Ubuntu bug tagged 'needs-packaging' with Wishlist importance.",
            "  https://bugs.launchpad.net/ubuntu/+filebug",
            "",
            &format!("  LP bug #{lp_bug} has been provided on the command line."),
        ],
    );
    if prompt_select(
        "Confirm the bug report exists and matches the provided LP bug number.",
        &["Confirmed — continue", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 2: Set Up Git Branch ───────────────────────────────────────────
    print_phase_header(2, "Set Up Git Branch");

    info!("fetching all remotes");
    git::fetch_all(repo_dir).await?;

    info!("checking out {old_merge_branch}");
    git::checkout_branch(repo_dir, &old_merge_branch).await?;

    info!("creating and pushing branch {merge_branch}");
    git::create_and_push_branch(repo_dir, &merge_branch, lpuser).await?;

    // ── Phase 3: Update Changelog and Package Name ───────────────────────────
    print_phase_header(3, "Update Changelog and Package Name");

    let deb_version = changelog::debian_version_string(new_ver);
    info!("running dch with version {deb_version}");
    changelog::run_dch(repo_dir, &deb_version).await?;

    let changelog_path = repo_dir.join("debian/changelog");
    info!("updating changelog entry");
    changelog::update_changelog_entry(
        &changelog_path,
        &old_pkg,
        &new_pkg,
        release,
        &new_ver.to_string(),
        lp_bug,
    )?;
    println!("  Changelog updated. First entry now:");
    let first_lines: String = std::fs::read_to_string(&changelog_path)?
        .lines()
        .take(6)
        .map(|l| format!("    {l}\n"))
        .collect();
    print!("{first_lines}");

    let watch_path = repo_dir.join("debian/watch");
    info!(
        "updating debian/watch version {} -> {}",
        old_short, new_short
    );
    uscan::update_watch_version(&watch_path, &old_short.to_string(), &new_short.to_string())?;
    println!("  debian/watch updated: {old_short} → {new_short}");

    // ── Phase 4: Temporarily Include All Vendored Dependencies ───────────────
    print_phase_header(4, "Temporarily Include All Vendored Dependencies");

    let copyright_path = repo_dir.join("debian/copyright");
    info!("commenting out vendor exclusion in debian/copyright");
    copyright::comment_out_vendor_exclusion(&copyright_path)?;
    println!("  debian/copyright patched (vendor line commented out — NOT committed).");

    // ── Phase 5: Get New Upstream Source (First Pass) ────────────────────────
    print_phase_header(5, "Get New Upstream Source (First Pass)");

    let log_dir = parent_dir.clone();
    let uscan_log = log_dir.join(format!("uscan-{new_ver}-first.log"));
    info!("running uscan --download-version {new_ver}");
    let old_tarball = uscan::run_uscan(repo_dir, new_ver, "~old", &uscan_log).await?;
    println!(
        "  Orig tarball (with full vendor): {}",
        old_tarball.display()
    );

    info!("restoring debian/copyright");
    git::restore_file(repo_dir, &repo_dir.join("debian/copyright")).await?;

    // ── Phase 6: Import Upstream Source into Git (First Pass) ────────────────
    print_phase_header(6, "Import Upstream Source into Git (First Pass)");

    info!("resetting experimental branch");
    git::reset_experimental_branch(repo_dir).await?;

    info!("running gbp import-orig with ~old tarball");
    gbp::gbp_import_orig(repo_dir, &old_tarball, "experimental", &merge_branch, &[]).await?;

    info!("creating safekeeping branch {import_old_branch}");
    git::create_branch(repo_dir, &import_old_branch).await?;
    git::push_branch(repo_dir, lpuser, &import_old_branch).await?;

    // ── Phase 7: Initial Patch Refresh ───────────────────────────────────────
    print_phase_header(7, "Initial Patch Refresh");
    run_interactive_patch_refresh(repo_dir).await?;

    // ── Phase 8: Prune Unwanted Vendored Dependencies ─────────────────────────
    print_phase_header(8, "Prune Unwanted Vendored Dependencies");

    info!("ensuring rustup is installed");
    vendor::ensure_rustup_installed().await?;

    info!("installing Rust toolchain {new_ver}");
    let rust_bootstrap_dir = vendor::rustup_install_toolchain(new_ver).await?;

    info!("installing cargo-vendor-filterer");
    vendor::install_cargo_vendor_filterer(new_ver).await?;

    info!("generating pruned vendor tarball");
    // Finding 6: pass new_ver so the tarball name is derived deterministically.
    let vendor_tarball =
        vendor::generate_vendor_tarball(repo_dir, &rust_bootstrap_dir, new_ver, "").await?;
    println!("  Vendor tarball: {}", vendor_tarball.display());

    // ── Phase 9: Remove Vendored C Libraries ──────────────────────────────────
    print_phase_header(9, "Remove Vendored C Libraries");
    run_interactive_c_library_removal(
        repo_dir,
        &rust_bootstrap_dir,
        &copyright_path,
        &vendor_tarball,
        new_ver,
    )
    .await?;

    // ── Phase 10: Update Source Tree Again (Second Pass) ─────────────────────
    print_phase_header(10, "Update Source Tree Again (Second Pass)");

    // Regenerate the main orig tarball without the vendor directory.
    let uscan_log2 = log_dir.join(format!("uscan-{new_ver}-final.log"));
    info!("running uscan again for final orig tarball");
    let final_tarball = uscan::run_uscan(repo_dir, new_ver, "", &uscan_log2).await?;
    println!("  Final orig tarball: {}", final_tarball.display());

    // Create a backup branch.
    let _ = git::create_branch(repo_dir, "backup").await;

    // Create import-new branch from old merge branch.
    git::checkout_branch(repo_dir, &old_merge_branch).await?;
    git::create_and_push_branch(repo_dir, &import_new_branch, lpuser).await?;

    // Cherry-pick the changelog entry commit.
    let changelog_commit =
        git::find_commit_by_message(repo_dir, &merge_branch, &format!("{new_ver}+dfsg")).await?;
    git::cherry_pick(repo_dir, &changelog_commit).await?;

    // Re-import with vendor component.
    git::reset_experimental_branch(repo_dir).await?;
    gbp::gbp_import_orig(
        repo_dir,
        &final_tarball,
        "experimental",
        &import_new_branch,
        &["--component=vendor"],
    )
    .await?;

    // Switch back and rebase.
    git::checkout_branch(repo_dir, &merge_branch).await?;
    print_info_box(
        "Manual step: interactive rebase required",
        &[
            "An interactive rebase is about to start.",
            "",
            "In the rebase editor, DROP the commit that imported the ~old tarball.",
            "It will look like:  'New upstream version <X.Y.Z>+dfsg~old'",
            "",
            "Keep all other commits (patch refreshes, changelog, etc.).",
        ],
    );
    if prompt_select(
        "Ready to start the interactive rebase?",
        &["Start rebase", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }
    // Finding 4: git rebase -i requires a real TTY — use run_interactive_command
    // so stdin/stdout/stderr are inherited rather than piped.
    crate::shell::run_interactive_command(
        "git",
        &["rebase", "-i", &import_new_branch],
        repo_dir,
        &[],
    )
    .await?;

    // Verify windows crates are stubbed.
    verify_windows_crate_stubs(repo_dir)?;

    // ── Phase 11: Update Versioned Package References ─────────────────────────
    print_phase_header(11, "Update Versioned Package References in Control Files");

    info!("running debian/rules update-version");
    control::run_update_version_rule(repo_dir, &rust_bootstrap_dir).await?;

    let control_path = repo_dir.join("debian/control");
    control::verify_bootstrap_deps(&control_path, &old_short, &new_short).await?;
    println!("  Bootstrap Build-Depends verified.");

    print_info_box(
        "Please review the changes",
        &[
            "Run: git diff",
            "Confirm rustc-<old> and rustc-<new> appear in Build-Depends.",
        ],
    );
    if prompt_select(
        "Confirm the diff looks correct.",
        &["Looks correct — commit", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    git::add_and_commit(
        repo_dir,
        &[
            "debian/control",
            "debian/control.in",
            "debian/source/lintian-overrides",
        ],
        &format!("Update versioned package references to {new_pkg}"),
    )
    .await?;

    // ── Phase 12: After-Repack Patch Refreshes ───────────────────────────────
    print_phase_header(12, "After-Repack Patch Refreshes");
    run_interactive_patch_refresh(repo_dir).await?;

    // ── Phase 13: Update XS-Vendored-Sources-Rust ────────────────────────────
    print_phase_header(13, "Update XS-Vendored-Sources-Rust");

    patches::quilt_push_all_unconditional(repo_dir).await?;

    let xs_value = control::generate_vendored_sources(repo_dir).await?;
    let windows_crates = control::check_no_windows_crates(&xs_value);
    if !windows_crates.is_empty() {
        print_info_box(
            "Warning: Windows crates found in XS-Vendored-Sources-Rust",
            &windows_crates
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
        );
        if prompt_select(
            "These crates should have been pruned. Continue anyway?",
            &["Continue anyway", "Abort"],
            1,
        ) != 0
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    control::update_xs_vendored_sources(&control_path, &xs_value)?;
    print_info_box(
        "Verify: empty line after XS-Vendored-Sources-Rust",
        &["Make sure there is still a blank line after the field in debian/control."],
    );
    if prompt_select(
        "Confirm the XS-Vendored-Sources-Rust field looks correct.",
        &["Confirmed — commit", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }
    git::add_and_commit(
        repo_dir,
        &["debian/control"],
        "Update XS-Vendored-Sources-Rust",
    )
    .await?;

    // ── Phase 14: Update Vendored Copyright Overrides ────────────────────────
    print_phase_header(14, "Update Vendored Copyright Overrides");

    info!("running debian/add-vendored-copyright-overrides");
    crate::shell::run_command(
        "debian/add-vendored-copyright-overrides",
        &[],
        repo_dir,
        &[],
    )
    .await?;
    git::add_and_commit(
        repo_dir,
        &["debian/source/lintian-overrides"],
        "Update vendored copyright overrides",
    )
    .await?;

    // ── Phase 15: Update debian/copyright ────────────────────────────────────
    print_phase_header(15, "Update debian/copyright");
    run_interactive_copyright_update(repo_dir).await?;

    // ── Phase 16: Local Build and Bug Fixing ─────────────────────────────────
    print_phase_header(16, "Local Build and Bug Fixing");
    if !run_interactive_local_build(repo_dir, &parent_dir, release).await? {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 17: Lintian Checks ─────────────────────────────────────────────
    print_phase_header(17, "Lintian Checks");
    run_interactive_lintian(repo_dir).await?;

    // ── Phase 18: PPA Build ───────────────────────────────────────────────────
    print_phase_header(18, "PPA Build");

    print_info_box(
        "Creating PPA",
        &[
            &format!("PPA name: {ppa_name}"),
            "",
            "After creation, you must manually:",
            "  1. Enable all processors (including RISC-V)",
            "  2. Set Ubuntu dependencies to 'Proposed'",
        ],
    );
    if prompt_select("Create PPA now?", &["Yes, create PPA", "Skip for now"], 0) == 0 {
        let ppa_url = ppa::create_ppa(&ppa_name).await?;
        if !ppa_url.is_empty() {
            println!("  PPA created: {ppa_url}");
        }
    }

    ppa::add_ppa_changelog_entry(repo_dir, &deb_version, release, 1).await?;
    build::run_dpkg_buildpackage_source(repo_dir).await?;

    // Find the .changes file.
    let changes_file = find_changes_file(&parent_dir)?;
    let ppa_ref = format!("{lpuser}/{ppa_name}");
    ppa::dput_to_ppa(&ppa_ref, &changes_file).await?;

    // Revert the temporary PPA changelog entry.
    git::restore_file(repo_dir, &changelog_path).await?;
    println!("  Temporary PPA changelog entry reverted.");

    // ── Phase 19: autopkgtests ────────────────────────────────────────────────
    print_phase_header(19, "autopkgtests");

    // Finding 13: integrate ppa::get_ppa_test_urls and autopkgtest qemu runs.
    let ppa_apt_source = format!(
        "deb [trusted=yes] https://ppa.launchpadcontent.net/{lpuser}/{ppa_name}/ubuntu/ {release} main"
    );
    println!("  PPA apt source: {ppa_apt_source}");
    let test_urls = ppa::get_ppa_test_urls(lpuser, &ppa_name, release).await?;
    print_info_box(
        "PPA autopkgtest URLs",
        &test_urls.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    );

    let image_dir = std::env::temp_dir().join("autopkgtest-images");
    std::fs::create_dir_all(&image_dir)?;
    let image_path = autopkgtest::build_testbed_image(release, 20, 2048, 2, &image_dir).await?;
    println!("  Testbed image: {}", image_path.display());

    let autopkgtest_log = parent_dir.join(format!("autopkgtest-{new_short}.log"));
    match autopkgtest::run_autopkgtest_qemu(
        &new_pkg,
        &image_path,
        &ppa_apt_source,
        &autopkgtest_log,
        2048,
        2,
    )
    .await
    {
        Ok(()) => println!("  autopkgtest passed."),
        Err(e) => {
            println!("  autopkgtest failed (standard): {e}");
            println!("  Retrying with big-packages resources (8192 MB / 4 CPUs)…");
            let big_image =
                autopkgtest::build_testbed_image(release, 20, 8192, 4, &image_dir).await?;
            autopkgtest::run_autopkgtest_qemu(
                &new_pkg,
                &big_image,
                &ppa_apt_source,
                &autopkgtest_log,
                8192,
                4,
            )
            .await?;
            println!("  autopkgtest passed with big-packages resources.");
        }
    }

    print_info_box(
        "PPA autopkgtests — next steps",
        &[
            "Click all autopkgtest links except i386 to trigger remaining arches.",
            "Re-run 'ppa tests' to check status.",
            "",
            "If tests fail with SIGKILL, add the package to autopkgtest-package-configs:",
            &format!("  rustc-{new_short}"),
        ],
    );
    if prompt_select(
        "Confirm autopkgtest results.",
        &["All autopkgtests complete — continue", "Abort"],
        0,
    ) != 0
    {
        println!("Aborted.");
        return Ok(());
    }

    // ── Phase 20: Upload the Package ─────────────────────────────────────────
    print_phase_header(20, "Upload the Package");

    info!("pushing {merge_branch} to {git_remote}");
    git::push_branch(repo_dir, git_remote, &merge_branch).await?;

    print_info_box(
        "Next steps for sponsorship",
        &[
            "Compile the following for your Launchpad bug comment:",
            "  • Link to your successfully-built PPA packages",
            &format!(
                "  • Link to branch: https://git.launchpad.net/~canonical-foundations/ubuntu/+source/rustc (merge-{new_short})"
            ),
            "  • Notable packaging changes (patches added/dropped, etc.)",
            "  • Output of: lintian",
            "  • Links to PPA autopkgtest build logs",
            "",
            "Then:",
            "  1. Ask an Archive Admin to add rustc-<X.Y> to the i386 allowlist.",
            "  2. Subscribe ubuntu-sponsors to the bug.",
            "  3. Contact Foundations/Toolchains team for timely sponsorship.",
            "",
            "After upload, update the Rust Toolchain Availability Page:",
            "  https://documentation.ubuntu.com/ubuntu-for-developers/reference/availability/rust/",
        ],
    );

    println!("\nthermite update complete.");
    Ok(())
}

// ── interactive helper loops ──────────────────────────────────────────────────

/// Interactive patch refresh loop. Repeats until all patches apply cleanly.
async fn run_interactive_patch_refresh(repo_dir: &Path) -> Result<()> {
    loop {
        match patches::quilt_push_all(repo_dir).await? {
            patches::QuiltResult::AllApplied => {
                println!("  All patches applied cleanly.");
                break;
            }
            patches::QuiltResult::PatchFailed {
                patch_name,
                conflicted_files,
            } => {
                // Finding 3: materialise conflict markers so the user can see
                // exactly what needs resolving.
                let force_result = patches::quilt_push_force_merge(repo_dir).await?;
                let conflict_list: Vec<String> = match force_result {
                    patches::QuiltResult::PatchFailed {
                        ref conflicted_files,
                        ..
                    } if !conflicted_files.is_empty() => conflicted_files
                        .iter()
                        .map(|p| format!("    {}", p.display()))
                        .collect(),
                    _ => conflicted_files
                        .iter()
                        .map(|p| format!("    {}", p.display()))
                        .collect(),
                };
                let mut lines = vec![
                    format!("Patch failed: {patch_name}"),
                    String::new(),
                    "Conflicted files (conflict markers written):".to_owned(),
                ];
                lines.extend(conflict_list);
                lines.extend([
                    String::new(),
                    "Resolution options:".to_owned(),
                    "  1. Surrounding code changed — verify and re-apply".to_owned(),
                    "  2. Patch implemented upstream — delete .patch and remove from series"
                        .to_owned(),
                    "  3. Vendored dependency changed — update file paths in .patch".to_owned(),
                    "  4. Code refactored — preserve patch intent, rewrite as needed".to_owned(),
                    String::new(),
                    "After resolving run: quilt refresh".to_owned(),
                ]);
                let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
                print_info_box("Action required: patch refresh", &line_refs);

                // Finding 3: ask whether the user refreshed or dropped the patch.
                match prompt_select(
                    "How did you resolve the patch conflict?",
                    &[
                        "Refreshed the patch (quilt refresh)",
                        "Dropped the patch",
                        "Abort",
                    ],
                    0,
                ) {
                    0 => {
                        patches::quilt_refresh(repo_dir).await?;
                        println!("  Patch refreshed.");
                    }
                    1 => {
                        println!("  Patch assumed dropped — continuing.");
                    }
                    _ => {
                        println!("Aborted.");
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}

/// Finding 12: accept copyright_path and version so the function can write
/// debian/copyright exclusions and debian/control Build-Depends entries.
async fn run_interactive_c_library_removal(
    repo_dir: &Path,
    rust_bootstrap_dir: &Path,
    copyright_path: &Path,
    vendor_tarball: &Path,
    version: &RustVersion,
) -> Result<()> {
    let c_files = uscan::list_c_files_in_tarball(vendor_tarball).await?;
    if c_files.is_empty() {
        println!("  No C source files found in the vendor tarball.");
        return Ok(());
    }

    println!("  C source files found in vendor tarball:");
    for f in &c_files {
        println!("    {f}");
    }

    print_info_box(
        "Review vendored C libraries",
        &[
            "Individual .c files are generally fine.",
            "Look for entire C libraries (e.g., libgit2/, oniguruma/, curl/).",
            "",
            "For each bundled C library you identify:",
            "  1. Add it to Files-Excluded-vendor in debian/copyright",
            "  2. Add the system package to Build-Depends in d/control and d/control.in",
            "  3. Patch the vendored crate's build.rs/Cargo.toml to use the system lib",
        ],
    );

    let control_path = repo_dir.join("debian/control");
    let control_in_path = repo_dir.join("debian/control.in");

    loop {
        if prompt_select(
            "Have you identified a bundled C library to remove?",
            &["Yes — add exclusion", "No more libraries — continue"],
            1,
        ) != 0
        {
            break;
        }
        // Finding 12: capture the exclusion pattern and build-dep interactively.
        let exclusion = prompt_input(
            "Enter the Files-Excluded-vendor pattern (e.g. vendor/libgit2-sys/libgit2/):",
        );
        if !exclusion.is_empty() {
            copyright::add_vendor_exclusion(copyright_path, &exclusion)?;
            println!("  Added exclusion '{exclusion}' to debian/copyright.");
        }
        let build_dep = prompt_input(
            "Enter the system Build-Depends package (e.g. libgit2-dev), or leave empty to skip:",
        );
        if !build_dep.is_empty() {
            control::add_build_dependency(&control_path, &build_dep)?;
            if control_in_path.exists() {
                control::add_build_dependency(&control_in_path, &build_dep)?;
            }
            println!("  Added Build-Depends '{build_dep}' to debian/control.");
        }
        // Finding 6: pass version so the tarball name is deterministic.
        vendor::generate_vendor_tarball(repo_dir, rust_bootstrap_dir, version, "").await?;
        println!("  Vendor tarball regenerated.");
    }
    Ok(())
}

/// Interactive debian/copyright update loop.
async fn run_interactive_copyright_update(repo_dir: &Path) -> Result<()> {
    let lintian_log = repo_dir.join("lintian-copyright.log");
    build::run_dpkg_buildpackage_source(repo_dir).await?;

    loop {
        let output =
            lintian::run_lintian(repo_dir, &["-i", "-I", "-E", "--pedantic"], &lintian_log).await?;

        // Find mismatched overrides.
        let mismatched: Vec<String> = output
            .raw
            .lines()
            .filter(|l| l.contains("mismatched-override file-without-copyright-information"))
            .map(|l| l.to_owned())
            .collect();

        if !mismatched.is_empty() {
            println!("  Removing {} mismatched overrides.", mismatched.len());
            let overrides_path = repo_dir.join("debian/source/lintian-overrides");
            if overrides_path.exists() {
                lintian::remove_mismatched_overrides(&overrides_path, &mismatched)?;
            }
        }

        // Generate missing copyright stanzas.
        let new_stanzas = lintian::run_lintian_to_copyright(repo_dir, &lintian_log).await;
        // Finding 14: collapse nested ifs into a single guard (clippy::collapsible_if).
        if let Ok(stanzas) = new_stanzas
            && !stanzas.trim().is_empty()
        {
            print_info_box(
                "Missing copyright stanzas",
                &[
                    "Add the following to debian/copyright in alphabetical order:",
                    "",
                    &stanzas,
                ],
            );
            if prompt_select(
                "Add the stanzas shown above, then continue.",
                &["Stanzas added — re-run Lintian", "Abort"],
                0,
            ) != 0
            {
                println!("Aborted.");
                return Ok(());
            }
            build::run_dpkg_buildpackage_source(repo_dir).await?;
            continue;
        }

        if output.errors.is_empty() && output.warnings.is_empty() {
            println!("  Lintian: no errors or warnings.");
            break;
        }

        print_info_box(
            "Remaining Lintian output",
            &[
                &format!("  Errors  : {}", output.errors.len()),
                &format!("  Warnings: {}", output.warnings.len()),
                "",
                "Address or add overrides for each issue.",
                "Known acceptable exceptions:",
                "  • E: field-too-long Vendored-Sources-Rust",
                "  • E: unknown-file-in-debian-source lintian-overrides.in",
                "  • W: unknown-field Vendored-Sources-Rust",
            ],
        );
        if prompt_select(
            "All Lintian issues addressed?",
            &["Yes, all fixed — continue", "No, fix more issues"],
            0,
        ) != 0
        {
            continue;
        }
        break;
    }
    Ok(())
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
    loop {
        build::clean_build_artifacts(parent_dir, repo_dir).await?;
        match build::run_sbuild(repo_dir, release, &[]).await? {
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

                let log_line = match log_path
                    .as_ref()
                    .and_then(|p| std::fs::canonicalize(p).ok())
                {
                    Some(abs) => format!("Build log: {}", abs.display()),
                    None => {
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
                    "Build failed",
                    &[
                        &log_line,
                        "",
                        "Search for 'stdout ----' in the log to find test failure output.",
                        "Check https://github.com/rust-lang/rust/issues for upstream issues.",
                        "",
                        "If sbuild placed you in an interactive shell, you can re-run tests:",
                        "  debian/rules override_dh_auto_test-arch RUSTBUILD_TEST_FLAGS=<path>",
                    ],
                );
                if !failures.is_empty() {
                    println!("  Extracted {} test failure section(s).", failures.len());
                }
                if prompt_select(
                    "Build failed. What would you like to do?",
                    &["Fix failure and retry", "Abort"],
                    0,
                ) != 0
                {
                    return Ok(false);
                }
            }
        }
    }
}

/// Interactive Lintian check loop.
async fn run_interactive_lintian(repo_dir: &Path) -> Result<()> {
    let log_path = repo_dir.join("lintian-final.log");
    build::run_dpkg_buildpackage_source(repo_dir).await?;

    loop {
        let output =
            lintian::run_lintian(repo_dir, &["-i", "--tag-display-limit", "0"], &log_path).await?;

        if output.errors.is_empty() && output.warnings.is_empty() {
            println!("  Lintian: no errors or warnings.");
            break;
        }

        print_info_box(
            "Lintian output",
            &[
                &format!("  Errors  : {}", output.errors.len()),
                &format!("  Warnings: {}", output.warnings.len()),
            ],
        );
        if prompt_select(
            "All Lintian issues addressed (add overrides as needed)?",
            &["Yes, all addressed — continue", "No, rebuild and re-check"],
            0,
        ) != 0
        {
            build::run_dpkg_buildpackage_source(repo_dir).await?;
            continue;
        }
        break;
    }

    // Extra lints.
    print_info_box(
        "Extra lints (pedantic/experimental/informational)",
        &["Running extra lints for awareness. Most do not need to be fixed."],
    );
    let extra_log = repo_dir.join("lintian-extra.log");
    let _ = lintian::run_lintian(repo_dir, &["-i", "-I", "-E", "--pedantic"], &extra_log).await;
    prompt_select(
        "Review extra lints if desired, then continue.",
        &["Continue"],
        0,
    );

    Ok(())
}

// ── small helpers ─────────────────────────────────────────────────────────────

/// Finding 5 and Finding 1: find the newest .changes file by mtime (not
/// first-match), and include the stdout field required by CommandFailed.
fn find_changes_file(parent_dir: &Path) -> Result<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(parent_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".changes"))
        .collect();

    // Sort newest-first by modification time; fall back to name order on error.
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

fn verify_windows_crate_stubs(repo_dir: &Path) -> Result<()> {
    let vendor_dir = repo_dir.join("vendor");
    if !vendor_dir.exists() {
        return Ok(());
    }
    let windows_crates: Vec<String> = std::fs::read_dir(&vendor_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("windows"))
        .collect();

    if windows_crates.is_empty() {
        println!("  No windows crates found in vendor/ (unexpected — stubs should be present).");
    } else {
        println!(
            "  Found {} windows crate stub(s). Spot-check that they have empty lib.rs files.",
            windows_crates.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    /// Finding 9: all workflow-critical tools must appear in REQUIRED_TOOLS.
    #[test]
    fn required_tools_contains_workflow_dependencies() {
        for tool in &[
            "rustup",
            "ppa",
            "autopkgtest",
            "autopkgtest-buildvm-ubuntu-cloud",
        ] {
            assert!(
                REQUIRED_TOOLS.contains(tool),
                "REQUIRED_TOOLS is missing '{tool}'"
            );
        }
    }

    /// Finding 5: find_changes_file returns the newest .changes file, not a
    /// random first-match.  We create two files and then use std::fs to update
    /// the mtime of one before asserting.
    #[test]
    fn find_changes_file_returns_newest() {
        let tmp = std::env::temp_dir().join(format!(
            "thermite-changes-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        let old_path = tmp.join("rustc-1.84_amd64.changes");
        let new_path = tmp.join("rustc-1.85_amd64.changes");
        fs::write(&old_path, "old").unwrap();
        // Write new_path after a tiny sleep so it gets a later mtime on most
        // file systems (resolution ≥ 1 ms on Linux tmpfs).
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
}
