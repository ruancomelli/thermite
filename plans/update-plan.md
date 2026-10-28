# Implementation Plan: `thermite update`

## Overview

The `thermite update` command automates the Ubuntu Rust toolchain packaging update workflow
described at https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/update-rust/.
It creates a new versioned `rustc-<X.Y>` Ubuntu source package from an upstream Rust release, starting
from the previous versioned package branch in the Foundations Launchpad Git repository:  https://code.launchpad.net/~canonical-foundations/ubuntu/+source/rustc/+git/rustc

The command is long-running and interactive. Some phases (e.g., patch refresh, C library removal,
copyright stanza authoring) require human judgment and cannot be fully automated. thermite handles
fully automatable phases end-to-end, while providing guided, interactive prompts and clear
resumption points for phases that require manual intervention.

---

## Reference Documentation

- Update workflow (official docs): https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/update-rust/
- Rust version strings: https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/rust-version-strings/

---

## CLI Interface

**Command:** `thermite update`

### Required Parameters

| Long flag                | Short | Type   | Description                                          |
|--------------------------|-------|--------|------------------------------------------------------|
| `--rust-update-version`  | `-u`  | String | Rust version being packaged, in `X.Y.Z` format       |
| `--rust-old-version`     | `-o`  | String | Rust version being replaced, in `X.Y.Z` format       |
| `--release`              | `-r`  | String | Target Ubuntu release adjective (e.g., `noble`)      |
| `--lpuser`               | `-l`  | String | Launchpad username (also used as personal remote name)|
| `--lp-bug-number`        | `-b`  | String | Launchpad bug ID number for this work                |

### Optional Parameters

| Long flag       | Short | Type   | Default       | Description                                            |
|-----------------|-------|--------|---------------|--------------------------------------------------------|
| `--git-remote`  | `-g`  | String | `foundations` | Local Git remote name for the Foundations rustc repo  |

### Derived (Internal) Values

These values are computed from parameters and are never accepted on the command line:

| Name                        | Derivation                                              |
|-----------------------------|---------------------------------------------------------|
| `rust_update_version_short` | `rust_update_version` with the `.Z` suffix removed      |
| `rust_old_version_short`    | `rust_old_version` with the `.Z` suffix removed         |

### Example Invocation

```
thermite update \
  --rust-update-version 1.85.1 \
  --rust-old-version 1.84.0 \
  --release noble \
  --lpuser jdoe \
  --lp-bug-number 2109761
```

---

## Project Structure

```
src/
  lib.rs                          # Re-exports top-level modules
  bin/
    thermite.rs                   # Binary entry point; CLI definition and dispatch
  commands/
    mod.rs
    update.rs                     # Orchestrates the full update workflow
  steps/
    mod.rs
    git.rs                        # Git operations: fetch, checkout, branch, push, rebase
    changelog.rs                  # dch invocations and changelog text manipulation
    copyright.rs                  # debian/copyright and lintian-overrides edits
    uscan.rs                      # uscan invocations and orig tarball management
    gbp.rs                        # gbp import-orig invocations
    patches.rs                    # quilt push / refresh operations
    vendor.rs                     # cargo-vendor-filterer and vendor-tarball rule
    control.rs                    # debian/control and debian/control.in edits
    build.rs                      # dpkg-buildpackage and sbuild invocations
    lintian.rs                    # lintian invocations and output parsing
    ppa.rs                        # ppa-dev-tools and dput invocations
    autopkgtest.rs                # autopkgtest and autopkgtest-buildvm invocations
  types/
    mod.rs
    versions.rs                   # RustVersion (X.Y.Z and X.Y), newtype wrappers
    ubuntu.rs                     # UbuntuRelease validated enum / newtype
    params.rs                     # UpdateParams struct
  error.rs                        # Unified error type via thiserror
```

### Binary Entry Point (`src/bin/thermite.rs`)

- Defines the top-level `Cli` struct and `Commands` enum using `clap` derive macros.
- Dispatches to `commands::update::run(params)` or `commands::backport::run(params)`.
- Initialises `tokio` runtime and `tracing` subscriber.

### Orchestrator (`src/commands/update.rs`)

- Contains `pub async fn run(params: UpdateParams) -> Result<()>`.
- Calls each workflow step in sequence.
- For interactive steps, prints a clearly formatted prompt and waits for the user to
  confirm before continuing.
- Supports a `--resume-from <phase>` flag (future enhancement; noted here for design
  awareness so the phase sequence is designed to be re-entrant).

---

## Key Data Structures

### `UpdateParams` (`src/types/params.rs`)

```rust
pub struct UpdateParams {
    pub rust_update_version: RustVersion,       // X.Y.Z
    pub rust_update_version_short: ShortRustVersion, // X.Y (derived)
    pub rust_old_version: RustVersion,          // X.Y.Z
    pub rust_old_version_short: ShortRustVersion, // X.Y (derived)
    pub release: UbuntuRelease,
    pub lpuser: String,
    pub git_remote: String,                     // defaults to "foundations"
    pub lp_bug_number: String,
}
```

Validation on construction:
- Both `RustVersion` values must be parseable as `X.Y.Z` where each component is a non-negative integer.
- `UbuntuRelease` must be a known Ubuntu release name (see below).
- `lp_bug_number` must be a non-empty string of digits.
- `lpuser` must be non-empty.

### `RustVersion` and `ShortRustVersion` (`src/types/versions.rs`)

```rust
/// A full Rust version in X.Y.Z format.
pub struct RustVersion { major: u32, minor: u32, patch: u32 }

/// A short Rust version in X.Y format.
pub struct ShortRustVersion { major: u32, minor: u32 }
```

- `RustVersion::parse(s: &str) -> Result<Self>` — parses `"X.Y.Z"`.
- `ShortRustVersion::from_full(v: &RustVersion) -> Self` — drops `.Z`.
- Both implement `Display` to produce canonical string representations.

### `UbuntuRelease` (`src/types/ubuntu.rs`)

Validated newtype around a `String`.  
`UbuntuRelease::parse(s: &str) -> Result<Self>` checks against the known list of Ubuntu release
adjectives.  For the `update` command, all current Ubuntu releases are valid targets.

The known release list is maintained as a `const` slice within this module and must be updated
as new Ubuntu releases are announced.

### `ThermiteError` (`src/error.rs`)

Defined with `thiserror`. Key variants:

```rust
pub enum ThermiteError {
    #[error("invalid Rust version string '{0}': expected X.Y.Z")]
    InvalidRustVersion(String),
    #[error("unknown Ubuntu release '{0}'")]
    UnknownRelease(String),
    #[error("command '{cmd}' failed with exit code {code}:\n{stderr}")]
    CommandFailed { cmd: String, code: i32, stderr: String },
    #[error("command '{0}' was not found on PATH")]
    CommandNotFound(String),
    #[error("patch refresh required manual intervention: {0}")]
    PatchRefreshRequired(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

---

## Workflow Phases

Each phase corresponds to one or more functions in the `steps/` module hierarchy.  
Phases that require manual intervention pause and display instructions to the user,
then await a confirmation keypress before continuing.

---

### Phase 0 — Preflight Checks

**Module:** `steps::git`, `steps::build`

1. Verify required external tools are on `PATH`:
   - `git`, `dch`, `uscan`, `gbp`, `quilt`, `dpkg-buildpackage`, `sbuild`, `lintian`,
     `cargo`, `rustup`, `snap`, `dput`, `ppa` (ppa-dev-tools), `autopkgtest`
2. Verify the current working directory contains a `debian/changelog` and `debian/watch`.
3. Print a summary of all resolved parameters and ask the user to confirm before proceeding.

**Key function signatures:**
```rust
pub async fn check_required_tools(tools: &[&str]) -> Result<()>
pub async fn verify_debian_package_root(dir: &Path) -> Result<()>
```

---

### Phase 1 — Create a Bug Report

**Module:** `steps::ppa` (or a future `steps::lp` module using the `lpcli` skill)

The type of bug report differs based on whether this version will be the `rust-defaults`
default for devel:

- **Default release:** Create a bug under `rust-defaults` on Launchpad.
- **Non-default release:** Create a general Ubuntu bug tagged `needs-packaging` with Wishlist importance.

At this stage thermite prints the guidance and the Launchpad URL and pauses for the user to
create the bug manually (or via the `lpcli` skill if integrated later).
The user then provides the bug number via the `--lp-bug-number` parameter (already captured
at CLI parse time), so this phase is primarily a documentation checkpoint.

---

### Phase 2 — Set Up Git Branch

**Module:** `steps::git`

```
git fetch --all
git checkout merge-<X.Y_old>
git checkout -b merge-<X.Y>
git push <lpuser> merge-<X.Y>
```

**Key function signatures:**
```rust
pub async fn fetch_all(repo_dir: &Path) -> Result<()>
pub async fn checkout_branch(repo_dir: &Path, branch: &str) -> Result<()>
pub async fn create_and_push_branch(repo_dir: &Path, branch: &str, remote: &str) -> Result<()>
```

---

### Phase 3 — Update Changelog and Package Name

**Module:** `steps::changelog`

1. Run `dch -v <X.Y.Z>+dfsg-0ubuntu1` to create a new changelog entry.
2. Edit `debian/changelog` to:
   - Replace the source package name from `rustc-<X.Y_old>` to `rustc-<X.Y>`.
   - Set the target distribution to `<release>`.
   - Add `* New upstream version <X.Y.Z> (LP: #<lp_bug_number>)` to the changelog body.

The final entry must match the canonical format:
```
rustc-<X.Y> (<X.Y.Z>+dfsg-0ubuntu1) <release>; urgency=medium

  * New upstream version <X.Y.Z> (LP: #<lp_bug_number>)

 -- <name> <email>  <RFC-2822 date>
```

**Key function signatures:**
```rust
pub async fn run_dch(repo_dir: &Path, version_str: &str) -> Result<()>
pub fn update_changelog_entry(
    changelog_path: &Path,
    old_pkg_name: &str,
    new_pkg_name: &str,
    release: &str,
    upstream_version: &str,
    lp_bug: &str,
) -> Result<()>
```

The changelog file is edited in-place using string manipulation (not by spawning an editor)
for the package name and distribution substitutions.

---

### Phase 4 — Temporarily Include All Vendored Dependencies

**Module:** `steps::copyright`

Comment out the `vendor` entry under `Files-Excluded` in `debian/copyright`:

```diff
-  vendor
+# vendor
```

This is a temporary, non-committed change that allows `uscan` to include the full
`vendor/` directory in the first orig tarball download.

**Key function signature:**
```rust
pub fn comment_out_vendor_exclusion(copyright_path: &Path) -> Result<()>
```

---

### Phase 5 — Get New Upstream Source (First Pass)

**Module:** `steps::uscan`

1. Run:
   ```
   uscan --download-version <X.Y.Z> -v --destdir <staging_dir> 2>&1 | tee <log_path>
   ```
   Capture and display `uscan` output. Log is saved to a file in the working directory.

2. Move the resulting orig tarball into the parent directory with the `~old` suffix:
   ```
   <staging_dir>/rustc-<X.Y>_<X.Y.Z>+dfsg.orig.tar.xz → ../rustc-<X.Y>_<X.Y.Z>+dfsg~old.orig.tar.xz
   ```
   uscan downloads into a private staging directory (`<parent>/.thermite/uscan-<version>-…`,
   removed afterwards), so the produced tarball is unambiguous even when several backports
   share the same worktree. The dfsg suffix in the produced name follows the watch file's
   `repacksuffix` (`+dfsg` for rustc-1.97 and newer, `+dfsg1` for older packages) and is
   normalised away: the tarball lands directly under its final name.

3. Restore `debian/copyright` to remove the temporary vendor comment:
   ```
   git restore debian/copyright
   ```

**Key function signatures:**
```rust
pub async fn run_uscan(repo_dir: &Path, version: &RustVersion, dfsg_suffix: &str, log_path: &Path) -> Result<PathBuf>
pub async fn git_restore_file(repo_dir: &Path, file: &Path) -> Result<()>
```

---

### Phase 6 — Import Upstream Source into Git (First Pass)

**Module:** `steps::gbp`, `steps::git`

1. Reset the `experimental` branch:
   ```
   git branch -D experimental
   git branch experimental
   ```
2. Import the `~old` tarball:
   ```
   gbp import-orig \
     --no-symlink-orig \
     --no-pristine-tar \
     --upstream-branch=experimental \
     --debian-branch=merge-<X.Y> \
     ../rustc-<X.Y>_<X.Y.Z>+dfsg~old.orig.tar.xz
   ```
3. Create a safekeeping branch and push it:
   ```
   git branch import-old-<X.Y>
   git push <lpuser> import-old-<X.Y>
   ```

**Key function signatures:**
```rust
pub async fn reset_experimental_branch(repo_dir: &Path) -> Result<()>
pub async fn gbp_import_orig(
    repo_dir: &Path,
    tarball: &Path,
    upstream_branch: &str,
    debian_branch: &str,
    extra_args: &[&str],
) -> Result<()>
```

---

### Phase 7 — Initial Patch Refresh

**Module:** `steps::patches`

This phase is **interactive**. The patch refresh loop cannot be fully automated because
each failing patch requires human judgment.

1. Run `quilt push -a`.
2. If all patches apply cleanly, proceed.
3. If a patch fails:
   a. Run `quilt push -f --merge` to force-apply with conflict markers.
   b. Display the list of conflicted files to the user.
   c. Pause and display guidance on how to resolve the conflict, covering the four
      common cases:
      - Surrounding code changed
      - Patch implemented upstream (drop the patch)
      - Vendored dependency changed (update file paths in `.patch`)
      - Targeted code refactored (preserve intent; may require rewriting)
   d. Wait for user confirmation that the conflict has been resolved.
   e. Run `quilt refresh` to update the patch.
   f. Repeat from step 1.

**Key function signatures:**
```rust
pub async fn quilt_push_all(repo_dir: &Path) -> Result<QuiltResult>
pub async fn quilt_push_force_merge(repo_dir: &Path) -> Result<QuiltResult>
pub async fn quilt_refresh(repo_dir: &Path) -> Result<()>

pub enum QuiltResult {
    AllApplied,
    PatchFailed { patch_name: String, conflicted_files: Vec<PathBuf> },
}
```

---

### Phase 8 — Prune Unwanted Vendored Dependencies

**Module:** `steps::vendor`

1. Ensure `rustup` is installed (prompt user to run `snap install rustup` if not found).
2. Install the matching Rust toolchain:
   ```
   rustup install <X.Y.Z>
   ```
3. Install `cargo-vendor-filterer`:
   ```
   cargo +<X.Y.Z> install cargo-vendor-filterer
   ```
4. Generate the pruned vendor tarball component:
   ```
   RUST_BOOTSTRAP_DIR=~/.rustup/toolchains/<X.Y.Z>-x86_64-unknown-linux-gnu \
     debian/rules vendor-tarball
   ```
   This produces `../rustc-<X.Y>_<X.Y.Z>+dfsg.orig-vendor.tar.xz`.

**Key function signatures:**
```rust
pub async fn ensure_rustup_installed() -> Result<()>
pub async fn rustup_install_toolchain(version: &RustVersion) -> Result<PathBuf>
pub async fn install_cargo_vendor_filterer(version: &RustVersion) -> Result<()>
pub async fn generate_vendor_tarball(
    repo_dir: &Path,
    rust_bootstrap_dir: &Path,
) -> Result<PathBuf>
```

---

### Phase 9 — Remove Vendored C Libraries

**Module:** `steps::vendor`, `steps::copyright`, `steps::control`

This phase is **interactive**. Deciding which C libraries to keep or replace requires
human judgment.

1. Scan the vendor tarball for C source files:
   ```
   tar -tJf ../rustc-<X.Y>_<X.Y.Z>+dfsg.orig-vendor.tar.xz | grep '\.c$'
   ```
2. Display the list of `.c` files found (individual files are generally fine; look for
   entire bundled libraries).
3. For each bundled C library identified by the user:
   a. Add the library directory to `Files-Excluded-vendor` in `debian/copyright`.
   b. Add the corresponding system package to `Build-Depends` in both `debian/control`
      and `debian/control.in`.
   c. Display guidance for patching the vendored crate's `build.rs` or `Cargo.toml`
      to prefer the system library.
   d. Wait for user confirmation that the patch has been created.
   e. Regenerate the vendor tarball (repeat Phase 8, step 4).
4. Repeat until the user confirms no more C libraries need to be removed.

**Key function signatures:**
```rust
pub async fn list_c_files_in_tarball(tarball: &Path) -> Result<Vec<String>>
pub fn add_vendor_exclusion(copyright_path: &Path, pattern: &str) -> Result<()>
pub fn add_build_dependency(control_path: &Path, dep: &str) -> Result<()>
```

---

### Phase 10 — Update Source Tree Again (Second Pass)

**Module:** `steps::uscan`, `steps::git`, `steps::gbp`

Now that the vendor tarball is pruned, regenerate the main orig tarball (without the
`vendor/` directory) and rebase all changes onto clean tarballs.

1. Verify `vendor` is listed under `Files-Excluded` (not `Files-Excluded-vendor`) in
   `debian/copyright`.
2. Run `uscan` again (without the vendor):
   ```
   uscan --download-version <X.Y.Z> -v --destdir <staging_dir> 2>&1 | tee <log_path>
   ```
3. Move the tarball to the canonical format (no suffix):
   ```
   <staging_dir>/rustc-<X.Y>_<X.Y.Z>+dfsg.orig.tar.xz → ../rustc-<X.Y>_<X.Y.Z>+dfsg.orig.tar.xz
   ```
4. Create a backup branch:
   ```
   git branch backup
   ```
5. Switch to `merge-<X.Y_old>`, create `import-new-<X.Y>`:
   ```
   git checkout merge-<X.Y_old>
   git checkout -b import-new-<X.Y>
   ```
6. Cherry-pick the changelog entry commit from `merge-<X.Y>`:
   ```
   git cherry-pick <commit_hash>
   ```
   thermite identifies this commit by searching `git log merge-<X.Y>` for the commit
   that added the `<X.Y.Z>+dfsg-0ubuntu1` changelog entry.

7. Reset `experimental` branch and import with the vendor component:
   ```
   git branch -D experimental
   git branch experimental
   gbp import-orig \
     --no-symlink-orig \
     --no-pristine-tar \
     --upstream-branch=experimental \
     --debian-branch=import-new-<X.Y> \
     --component=vendor \
     ../rustc-<X.Y>_<X.Y.Z>+dfsg.orig.tar.xz
   ```
8. Rebase `merge-<X.Y>` onto `import-new-<X.Y>`, dropping the `~old` tarball import commit:
   ```
   git checkout merge-<X.Y>
   git rebase -i import-new-<X.Y>
   ```
   thermite prints clear instructions to the user about which commit to `drop` in the
   interactive rebase editor, then pauses for confirmation.

**Key function signatures:**
```rust
pub async fn find_changelog_commit(repo_dir: &Path, branch: &str, version_str: &str) -> Result<String>
pub async fn cherry_pick(repo_dir: &Path, commit: &str) -> Result<()>
pub async fn interactive_rebase(repo_dir: &Path, onto: &str) -> Result<()>
```

---

### Phase 11 — Update Versioned Package References in Control Files

**Module:** `steps::control`

Run `debian/rules update-version` with the installed Rust bootstrap toolchain:
```
RUST_BOOTSTRAP_DIR=~/.rustup/toolchains/<X.Y.Z>-x86_64-unknown-linux-gnu \
  debian/rules update-version
```

After the script completes, display `git diff` output for the user to verify that in
`debian/control` the two `Build-Depends` bootstrapping compiler entries are
`rustc-<X.Y_old>` and `rustc-<X.Y>`.  Wait for user confirmation before committing.

Then commit:
```
git add debian/control debian/control.in debian/source/lintian-overrides*
git commit -m "Update versioned package references to rustc-<X.Y>"
```

**Key function signatures:**
```rust
pub async fn run_update_version_rule(repo_dir: &Path, rust_bootstrap_dir: &Path) -> Result<()>
pub async fn verify_bootstrap_deps(control_path: &Path, old: &ShortRustVersion, new: &ShortRustVersion) -> Result<()>
```

---

### Phase 12 — After-Repack Patch Refreshes

**Module:** `steps::patches`

Repeat the interactive patch refresh loop from Phase 7. The most common case here is
dropping patches of pruned vendored files.

---

### Phase 13 — Update `XS-Vendored-Sources-Rust`

**Module:** `steps::control`

1. Push all patches to apply them to the source tree:
   ```
   quilt push -a
   ```
2. Generate the new `XS-Vendored-Sources-Rust` field value:
   ```
   CARGO_VENDOR_DIR=vendor/ /usr/share/cargo/bin/dh-cargo-vendored-sources
   ```
3. Replace the existing `XS-Vendored-Sources-Rust` field in `debian/control` with the
   new value.
4. Verify that no Windows crates appear in the new value (display them if found and
   warn the user).
5. Verify that the empty line after the field is preserved.
6. Commit the change.

**Key function signatures:**
```rust
pub async fn generate_vendored_sources(repo_dir: &Path) -> Result<String>
pub fn update_xs_vendored_sources(control_path: &Path, new_value: &str) -> Result<()>
pub fn check_no_windows_crates(xs_value: &str) -> Vec<String>
```

---

### Phase 14 — Update Vendored Copyright Overrides

**Module:** `steps::copyright`

Run the provided script to update `debian/source/lintian-overrides`:
```
debian/add-vendored-copyright-overrides
```

Commit the updated overrides file.

---

### Phase 15 — Update `debian/copyright`

**Module:** `steps::lintian`, `steps::copyright`

1. Build the source package:
   ```
   dpkg-buildpackage -S -I -i -nc -d -sa
   ```
2. Run Lintian and save output:
   ```
   lintian -i -I -E --pedantic | tee <lintian_results_path>
   ```
3. Identify unnecessary overrides:
   ```
   grep 'mismatched-override file-without-copyright-information' <lintian_results_path>
   ```
   Remove listed overrides from `debian/source/lintian-overrides`.

4. Identify missing copyright stanzas:
   ```
   cat <lintian_results_path> | debian/lintian-to-copyright.sh
   ```
   Display the generated stanzas and pause for the user to add them to `debian/copyright`
   in alphabetical order.

5. Rebuild and re-run Lintian after each round of changes until no new copyright
   warnings appear.

**Key function signatures:**
```rust
pub async fn run_dpkg_buildpackage_source(repo_dir: &Path) -> Result<()>
pub async fn run_lintian(repo_dir: &Path, flags: &[&str], log_path: &Path) -> Result<LintianOutput>
pub async fn run_lintian_to_copyright(repo_dir: &Path, lintian_log: &Path) -> Result<String>
pub fn remove_mismatched_overrides(overrides_path: &Path, entries: &[String]) -> Result<()>

pub struct LintianOutput {
    pub errors: Vec<LintianEntry>,
    pub warnings: Vec<LintianEntry>,
    pub raw: String,
}
```

---

### Phase 16 — Local Build and Bug Fixing

**Module:** `steps::build`

1. Remove previous build artifacts from the parent directory:
   ```
   rm -vf ../*.{debian.tar.xz,dsc,buildinfo,changes,ppa.upload}
   rm -vf debian/files
   rm -rf .pc
   ```
2. Run `sbuild`:
   ```
   sbuild -Ad <release>
   ```
   If a PPA is needed to bootstrap, add the `--extra-repository` option.

3. If the build fails:
   - Display the relevant `stdout ----` sections from the `sbuild` log.
   - Pause and prompt the user to fix the failure (with guidance on using the
     interactive `sbuild` shell, rerunning individual tests, and creating DEP-3 patches).
   - Repeat from step 2 after the user confirms the fix.

**Key function signatures:**
```rust
pub async fn clean_build_artifacts(parent_dir: &Path, repo_dir: &Path) -> Result<()>
pub async fn run_sbuild(repo_dir: &Path, release: &str, extra_args: &[String]) -> Result<SbuildResult>
pub fn extract_test_failures(sbuild_log: &Path) -> Result<Vec<String>>

pub enum SbuildResult {
    Success,
    Failure { log_path: PathBuf },
}
```

---

### Phase 17 — Lintian Checks

**Module:** `steps::lintian`

1. Build the source package.
2. Run standard Lintian:
   ```
   lintian -i --tag-display-limit 0 2>&1 | tee <log_path>
   ```
3. Display all errors and warnings. Pause and prompt the user to address or add
   overrides for each one, with guidance on the known acceptable exceptions
   (e.g., `field-too-long Vendored-Sources-Rust`, `unknown-file-in-debian-source`).
4. Run extra lints:
   ```
   lintian -i -I -E --pedantic
   ```
5. Iterate until the user confirms Lintian is satisfied.

---

### Phase 18 — PPA Build

**Module:** `steps::ppa`

1. Create a new PPA using `ppa-dev-tools`:
   ```
   ppa create rustc-<X.Y>-merge
   ```
   Display the PPA URL and remind the user to:
   - Enable all processors (including RISC-V).
   - Set Ubuntu dependencies to "Proposed" for a new versioned package.

2. Add a temporary PPA changelog entry:
   ```
   dch -bv <X.Y.Z>+dfsg-0ubuntu1~ppa1 \
     --distribution "<release>" \
     "PPA upload"
   ```
   (Increment `~ppaN` counter on subsequent uploads.)

3. Build the source package:
   ```
   dpkg-buildpackage -S -I -i -nc -d -sa
   ```

4. Upload to PPA:
   ```
   dput ppa:<lpuser>/rustc-<X.Y>-merge <path_to_source_changes>
   ```

5. Monitor PPA build status and prompt the user to fix any architecture-specific
   failures.  Display a link to the PPA build page.

After the PPA upload, `git restore debian/changelog` to remove the temporary PPA
changelog entry.

**Key function signatures:**
```rust
pub async fn create_ppa(name: &str) -> Result<String>
pub async fn add_ppa_changelog_entry(
    repo_dir: &Path,
    version_str: &str,
    release: &str,
    upload_number: u32,
) -> Result<()>
pub async fn dput_to_ppa(ppa_path: &str, changes_file: &Path) -> Result<()>
```

---

### Phase 19 — autopkgtests

**Module:** `steps::autopkgtest`

1. If test beds have not yet been created, guide the user through
   `autopkgtest-buildvm-ubuntu-cloud` for both `default` and `big` beds.

2. Run autopkgtests against the default test bed:
   ```
   autopkgtest rustc-<X.Y> \
     --apt-upgrade --shell-fail \
     --add-apt-source=ppa:<lpuser>/rustc-<X.Y>-merge \
     --log-file=<log_path> \
     -- qemu --ram-size=4096 --cpus=2 <default_img>
   ```

3. If autopkgtests pass, proceed to Phase 20.

4. If autopkgtests fail with SIGKILL (OOM), run against the `big` bed:
   ```
   autopkgtest rustc-<X.Y> \
     --apt-upgrade --shell-fail \
     --add-apt-source=ppa:<lpuser>/rustc-<X.Y>-merge \
     --log-file=<log_path> \
     -- qemu --ram-size=8192 --cpus=4 <big_img>
   ```

5. If the `big` bed passes, guide the user to create a merge proposal to add
   `rustc-<X.Y>` to the `big_packages` list in `autopkgtest-package-configs`.

6. Run PPA autopkgtests via `ppa-dev-tools`:
   ```
   ppa tests ppa:<lpuser>/rustc-<X.Y>-merge --release <release> --show-url
   ```
   Display the URLs; prompt the user to click each (except i386) and monitor results.

---

### Phase 20 — Upload the Package

**Module:** `steps::git`, `steps::ppa`

1. Collect upload info:
   - PPA build links
   - Link to `merge-<X.Y>` branch in the Foundations repo
   - Notable packaging changes summary (prompted from user)
   - Saved Lintian output
   - autopkgtest build log links

2. Push the branch to the Foundations remote:
   ```
   git push <git_remote> merge-<X.Y>
   ```

3. Display a formatted upload comment draft for the user to post to the Launchpad bug.
   Include all collected info in the format matching the canonical example.

4. Remind the user to:
   - Ask an Archive Admin to add `rustc-<X.Y>` to the i386 allowlist.
   - Subscribe `ubuntu-sponsors` to the bug.
   - Reach out to the Foundations or Toolchains team for timely sponsorship.

5. After sponsorship and upload, remind the user to update the
   [Rust Toolchain Availability Page](https://documentation.ubuntu.com/ubuntu-for-developers/reference/availability/rust/).

---

## Shell Command Execution

All external commands are run via a thin `run_command` helper in a shared internal
module (`src/shell.rs` or similar):

```rust
pub async fn run_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<CommandOutput>

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: ExitStatus,
}
```

- Streams stdout/stderr to the terminal in real time (using `tokio::process::Command`
  with piped output and a `tokio::io::BufReader` read loop).
- Logs the full command line and exit code via `tracing`.
- Returns `ThermiteError::CommandFailed` on non-zero exit codes.

---

## User Interaction Model

Interactive phases use a consistent pattern:

```
┌──────────────────────────────────────────────────────────────────┐
│  THERMITE — Phase 7: Initial Patch Refresh                       │
│  Action required                                                  │
├──────────────────────────────────────────────────────────────────┤
│  The following patch failed to apply:                            │
│    d-0050-disable-network-tests.patch                            │
│                                                                  │
│  Conflicted files:                                               │
│    src/tools/cargo/tests/testsuite/net.rs                        │
│                                                                  │
│  Resolution options:                                             │
│  1. Surrounding code changed — verify and re-apply              │
│  2. Patch implemented upstream — drop the patch                  │
│  3. Vendored dependency changed — update file paths in .patch    │
│  4. Targeted code refactored — preserve patch intent             │
│                                                                  │
│  After resolving, run: quilt refresh                             │
│                                                                  │
│  Press Enter when done, or type 'skip' to mark for later review. │
└──────────────────────────────────────────────────────────────────┘
```

This is implemented via a `prompt_user` helper in `src/ui.rs`.

---

## Error Handling Strategy

- All public functions return `Result<T, ThermiteError>`.
- Errors propagate using `?`.
- The top-level `run` function in `commands/update.rs` catches errors, prints a
  formatted error message with guidance on how to recover and resume, then exits
  with a non-zero status code.
- Errors from external commands include the command's stderr output to aid diagnosis.

---

## Async Execution Model

- The binary uses `#[tokio::main]` with a multi-threaded runtime.
- All I/O-bound work (command execution, file reads/writes) is `async`.
- Phases execute sequentially (the workflow is inherently serial).
- CPU-bound work (tarball inspection, string parsing) uses blocking tasks via
  `tokio::task::spawn_blocking`.

---

## Dependencies

| Crate             | Purpose                                                       |
|-------------------|---------------------------------------------------------------|
| `clap`            | CLI argument parsing (derive feature)                         |
| `tokio`           | Async runtime and process execution                           |
| `thiserror`       | Custom error type derivation                                  |
| `tracing`         | Structured logging                                            |
| `tracing-subscriber` | Logging initialisation                                     |
| `serde`           | Serialisation (for future config file support)                |
| `serde_json`      | JSON output / GitHub API calls                                |

All dependencies are added to `Cargo.toml` only as they are implemented.

---

## Testing Plan

### Unit Tests (`src/types/versions.rs`, `src/types/ubuntu.rs`)

- `RustVersion::parse` accepts valid `X.Y.Z` strings and rejects invalid ones.
- `ShortRustVersion::from_full` truncates correctly.
- `UbuntuRelease::parse` accepts known releases and rejects unknown ones.
- Changelog entry formatting produces the canonical string.
- `check_no_windows_crates` correctly identifies Windows crate names in the
  `XS-Vendored-Sources-Rust` field.

### Integration Tests (`test/`)

- Mock external commands using test doubles (a configurable `run_command` injected
  via trait or function pointer).
- Verify that each phase calls the expected commands with the correct arguments and
  environment variables.
- Verify file mutation functions (`comment_out_vendor_exclusion`,
  `update_xs_vendored_sources`, etc.) produce correct output for representative inputs.

### Manual / End-to-End Testing

Full end-to-end testing requires the Foundations Launchpad Git repository and an Ubuntu
build environment.  This is documented in `test/README.md` (to be created) and covers
running `thermite update` against a real (or snapshot) Rust release in a dedicated test
Ubuntu VM.

---

## Implementation Order

The recommended implementation sequence, following idiomatic Rust practices of building
the foundation before higher-level logic:

1. `src/error.rs` — `ThermiteError`
2. `src/types/versions.rs` — `RustVersion`, `ShortRustVersion`
3. `src/types/ubuntu.rs` — `UbuntuRelease`
4. `src/types/params.rs` — `UpdateParams`
5. `src/shell.rs` — `run_command`, `CommandOutput`
6. `src/ui.rs` — `prompt_user`, formatted output helpers
7. `src/bin/thermite.rs` — CLI skeleton (`clap`) that parses and validates `UpdateParams`
8. `src/steps/git.rs` — Git operations
9. `src/steps/changelog.rs` — dch and changelog manipulation
10. `src/steps/copyright.rs` — debian/copyright edits
11. `src/steps/uscan.rs` — uscan and tarball management
12. `src/steps/gbp.rs` — gbp import-orig
13. `src/steps/patches.rs` — quilt operations
14. `src/steps/vendor.rs` — rustup, cargo-vendor-filterer, vendor-tarball rule
15. `src/steps/control.rs` — debian/control and update-version rule
16. `src/steps/lintian.rs` — lintian and lintian-to-copyright.sh
17. `src/steps/build.rs` — dpkg-buildpackage and sbuild
18. `src/steps/ppa.rs` — ppa-dev-tools and dput
19. `src/steps/autopkgtest.rs` — autopkgtest
20. `src/commands/update.rs` — orchestrator wiring all phases together
21. Integration tests for each step module
