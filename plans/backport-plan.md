# Implementation Plan: `thermite backport`

## Overview

The `thermite backport` command automates the Ubuntu Rust toolchain backporting workflow
described in the official Ubuntu docs at
https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/.
A structured, AI-generated formalisation of those docs lives in
`thermite/docs/rust-backporting-runbook.md`; it is kept in sync with the official docs and
is subordinate to them.

It adapts an existing versioned `rustc-<X.Y>` Ubuntu source package for an older Ubuntu
release, starting from the package branch for the source release in the Foundations
Launchpad Git repository:
https://code.launchpad.net/~canonical-foundations/ubuntu/+source/rustc/+git/rustc

Like `thermite update`, the command is long-running and interactive. Fully automatable
phases run end-to-end; phases requiring human judgment pause with guided prompts.

The key complexity in backporting is that an older Ubuntu release may have older versions
of build dependencies (LLVM, libgit2, cmake, pkgconf, dh-cargo, etc.), requiring manual
fixes during Phase 8.  thermite cannot automate these since they are situation-specific,
but it documents the common fixes inline and links to the reference documentation.

---

## Reference Documentation

- **Backport workflow (official docs, primary):** https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/
- **Backport runbook** (AI-generated formalisation, subordinate to the official docs): `thermite/docs/rust-backporting-runbook.md`
- Rust version strings: https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/rust-version-strings/

When the runbook and the official docs disagree, follow the official docs; the runbook
should be corrected to match.

---

## CLI Interface

**Command:** `thermite backport`

### Required Parameters

| Long flag           | Short | Type   | Description                                            |
|---------------------|-------|--------|--------------------------------------------------------|
| `--rust-version`    | `-u`  | String | Rust version to backport, in `X.Y.Z` format            |
| `--source-release`  | `-s`  | String | Ubuntu release to backport FROM (e.g. `noble`)         |
| `--release`         | `-r`  | String | Ubuntu release to backport TO (e.g. `jammy`)           |
| `--lpuser`          | `-l`  | String | Launchpad username (also used as personal remote name) |

### Optional Parameters

| Long flag          | Short | Type   | Default        | Description                                     |
|--------------------|-------|--------|----------------|-------------------------------------------------|
| `--lp-bug-number`  | `-b`  | String | (none)         | LP bug ID; omit for proactive backports          |
| `--git-remote`     | `-g`  | String | `foundations`  | Local Git remote for the Foundations rustc repo  |
| `--repo-dir`       | `-d`  | Path   | current dir    | Root of the Debian source package               |

---

## Validated Parameter Type: `BackportParams`

Defined in `src/types/params.rs`.

Fields:
- `rust_version: RustVersion` — parsed from `--rust-version`
- `source_release: UbuntuRelease` — parsed from `--source-release`
- `release: UbuntuRelease` — parsed from `--release`
- `lpuser: String` — must be non-empty
- `git_remote: String` — defaults to `"foundations"`
- `lp_bug_number: Option<String>` — when `Some`, must be digits-only

Validation errors:
- `InvalidLpUser` if `lpuser` is empty
- `InvalidLpBugNumber` if `lp_bug_number` is `Some` but not digits-only
- `InvalidBackportReleases` if `source_release == release`
- `InvalidRustVersion` / `UnknownRelease` from the inner type parsers

---

## New Helpers Added to Existing Steps

### `src/types/ubuntu.rs` — `UbuntuRelease::series_number()`

Returns the numeric series identifier for the release (e.g. `"jammy"` → `"22.04"`).
Used by `compute_backport_version()`.  The mapping is stored as a `&[(&str, &str)]`
constant alongside the existing release list.

### `src/steps/changelog.rs`

Three new functions:

**`read_current_version(changelog_path) -> Result<String>`**
Reads the version string from the first line of `debian/changelog`.
Format: `<pkg> (<version>) <dist>; urgency=<level>` — extracts the text between `(` and `)`.

**`compute_backport_version(current_version, target_series) -> String`**
Pure function. Algorithm:
1. Split `current_version` on the first `-` to get `(upstream, debian_rev)`.
2. Strip any trailing `~XX.YY[.Z…]` suffix from `upstream`, append `~<target_series>`.
3. Strip any trailing `~XX.YY[.Z…]` suffix from `debian_rev`, append `~<target_series>.1`.

Examples from the docs:
- `"1.93.0+dfsg-0ubuntu1"` + `"24.04"` → `"1.93.0+dfsg~24.04-0ubuntu1~24.04.1"`
- `"1.89.0+dfsg2~24.04.1-0ubuntu3~24.04.2"` + `"22.04"` → `"1.89.0+dfsg2~22.04-0ubuntu3~22.04.1"`

The suffix detection (`is_series_like`) only matches strings that consist entirely of
ASCII digits and dots, and start and end with a digit — so pre-release markers like
`~exp` are not stripped.

**`update_backport_changelog_entry(changelog_path, release, lp_bug) -> Result<()>`**
Edits the first entry of `debian/changelog` in-place:
- Replaces the distribution token in the first line with `release`.
- Replaces the first bullet with `"* Backport to <release> (LP: #N)"` (LP part omitted
  when `lp_bug` is `None`).

### `src/steps/build.rs` — `disable_self_build_test(repo_dir) -> Result<()>`

Removes the three-line self-build block from `debian/tests/control`:
```
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
Depends: @, @builddeps@
Restrictions: rw-build-tree, allow-stderr
```
The function is a no-op if the file does not exist or the block is already absent.
After removal, any leading blank lines left at the top of the file are stripped.

### `src/steps/ppa.rs` — `get_staging_ppa_test_urls(pkg_name, release) -> Result<Vec<String>>`

Runs:
```
ppa tests ppa:rust-toolchain/staging -p <pkg_name> --release <release> --show-url
```
Returns the list of `http…` lines from stdout.

---

## Workflow Phases

### Phase 0 — Preflight Checks (Automated)

- Check required tools on PATH: `git`, `dch`, `uscan`, `quilt`, `dpkg-buildpackage`,
  `lintian`, `sbuild`, `cargo`, `rustup`, `dput`, `ppa`.
- `git::verify_debian_package_root()` — check `debian/changelog` and `debian/watch` exist.
- Print parameter summary and ask for confirmation.

### Phase 1 — Create a Bug Report (Interactive)

- If `lp_bug_number` was provided: acknowledge it and note how to file one if needed.
- If not provided: explain this is a proactive backport; note that all backports go to the
  staging PPA regardless.
- `prompt_continue`.

### Phase 2 — Set Up Git Branch (Automated)

- `git fetch --all`
- `git checkout <source_release>-<X.Y>` (e.g. `noble-1.85`)
- `git checkout -b <release>-<X.Y>` (e.g. `jammy-1.85`)

The branch is pushed to the remote only in Phase 14, once the package has passed the
staging PPA autopkgtests.

### Phase 3 — Update Changelog (Automated)

- `changelog::read_current_version()` — reads the current version from `debian/changelog`.
- `changelog::compute_backport_version()` — derives the backport version.
- Print both and ask the user to confirm the computed version.
- `changelog::run_dch(repo_dir, new_version)` — creates the new entry headlessly.
- `changelog::update_backport_changelog_entry()` — sets distribution and bullet.
- Print the first 6 lines of the updated changelog for review.

### Phase 4 — Compatibility Checks (Interactive)

- `compat::run_all_checks(repo_dir, release)` — for each of LLVM, libgit2, dh-cargo,
  pkgconf, cmake, and debhelper-compat, infer the version the source package needs and
  check whether the target release's archive can satisfy it (via `rmadison -u ubuntu` and
  `dpkg --compare-versions`).
- Print a per-gate table (`OK` / `Needs change` / `Could not infer` / `Check failed`) and
  guide the user through applying any required changes manually.
- The outcome (especially LLVM or libgit2 vendoring, which change `Files-Excluded` in
  `debian/copyright`) determines the tarball decisions in Phases 5 and 6.

### Phase 5 — Provide Orig Tarball (Interactive)

Decision gate offering Reuse / Download / Regenerate / Abort:

- **Reuse** — no `Files-Excluded` change and the tarball already exists in the parent
  directory as `rustc-<X.Y>_<X.Y.Z>+dfsg~<series>.orig.tar.xz`.
- **Download** — attempt an automated fetch (`tarball_fetch::fetch_backport_tarball`):
  1. the rust-toolchain staging PPA on Launchpad, whose published source files already
     carry the `~<series>` suffix of a previous backport upload;
  2. the primary Ubuntu archive, reusing the orig tarball published for the same upstream
     version (candidate filenames resolved via `rmadison -u ubuntu rustc-<X.Y>`).
  Both are served by Launchpad `+files` URLs and downloaded with `wget` (or `curl`).
  When no candidate has the file, fall back to a manual-placement prompt.
- **Regenerate** — required when `Files-Excluded` changed in Phase 4:
  `uscan::run_uscan(repo_dir, rust_ver, "~<series>", log)` (20–60 minutes) lands the
  tarball directly as `rustc-<X.Y>_<X.Y.Z>+dfsg~<series>.orig.tar.xz`.
- **Abort**.

### Phase 6 — Provide Orig-Vendor Tarball (Interactive)

Same decision gate for `rustc-<X.Y>_<X.Y.Z>+dfsg~<series>.orig-vendor.tar.xz`:

- **Reuse** — the vendor tarball already exists locally.
- **Download** — same automated fetch (staging PPA, then Ubuntu archive) with a
  manual-placement fallback.
- **Regenerate** — delete any stale series-suffixed vendor tarballs, install the matching
  toolchain via `vendor::rustup_install_toolchain(rust_ver)`, then
  `vendor::generate_vendor_tarball()` running `debian/rules vendor-tarball` with
  `RUST_BOOTSTRAP_DIR`.
- **Abort**.

### Phase 7 — Disable Autopkgtest Self-Build Test (Automated)

- `build::disable_self_build_test(repo_dir)` — removes the three-line block from
  `debian/tests/control`.
- Check `git status --porcelain debian/tests/control`: if modified, commit with message
  `"Disable autopkgtest self-build test for backport"`.

Rationale: the self-build test is resource-intensive and likely to time out on the
autopkgtest infrastructure, especially for backports that vendor LLVM.

### Phase 8 — Local Build and Bug Fixing (Interactive)

Loop:
1. `build::quilt_pop_all(repo_dir)` then `build::clean_build_artifacts(parent_dir, repo_dir)`
2. `build::run_sbuild(repo_dir, release, extra_args)` with
   `--extra-repository=deb [trusted=yes] http://ppa.launchpad.net/rust-toolchain/staging/ubuntu/ <release> main`
   so the bootstrap compiler (`rustc-<X.Y>_old`, `cargo-<X.Y>_old`) resolves from the
   staging PPA:
   - `SbuildResult::Success` → break
   - `SbuildResult::Failure` → print info-box with build log path and common fix guidance;
     `prompt_continue`; loop.

The info-box lists the most common backporting issues (LLVM, libgit2, dh-cargo, pkgconf,
cmake, OpenSSL) with brief fix instructions and a link to the full backporting guide.

### Phase 9 — Build Source Package (Interactive)

- Expected `.dsc` path is deterministic: `<parent>/rustc-<X.Y>_<version>.dsc`.
- `prompt_select`: build now (`build::quilt_pop_all`, `clean_build_artifacts`,
  `build::run_dpkg_buildpackage_source`) or skip when the `.dsc` already exists.
- Skipping without a `.dsc` is only safe when Phase 10 will also be skipped.

### Phase 10 — Lintian Checks (Interactive)

- Skippable. Otherwise `lintian::run_lintian(repo_dir, ["-i", "--tag-display-limit", "0"], log)`
  writes `lintian-<X.Y>-<release>.log` and reports error/warning counts.
- Print info-box with the expected (ignorable) tags for rustc backports; all other issues
  must be fixed or overridden in `debian/source/lintian-overrides{,.in}`.

### Phase 11 — PPA Build (Interactive)

- Print info-box with suggested PPA name (`rustc-<X.Y>-<release>`).
- Optionally create the PPA via `ppa::create_ppa()`.
- `ppa::add_ppa_changelog_entry(repo_dir, new_version, release, 1)` — adds `~ppa1`.
- `build::run_dpkg_buildpackage_source(repo_dir)`.
- `find_changes_file(parent_dir)` → `ppa::dput_to_ppa("<lpuser>/<ppa_name>", changes)`.
- `git::restore_file(repo_dir, debian/changelog)` — reverts the `~ppa1` entry.
- Print PPA URL and `prompt_continue`.

### Phase 12 — Staging PPA Upload (Interactive)

- Print info-box explaining what the changelog description should contain (list of
  backporting changes made).
- `prompt_continue` then `run_interactive_command("dch", ["-r", "--no-auto-nmu"])` — open
  the user's configured editor with `dch -r` to finalize the changelog entry.
- Clean artifacts, then `build::run_dpkg_buildpackage_source(repo_dir)`.
- `find_changes_file(parent_dir)` → `ppa::dput_to_ppa("rust-toolchain/staging", changes)`
  (honours `--dry-run` via `confirm_sink`).
- Print staging PPA build-monitoring URL and confirm the build completed on all
  architectures before continuing.

### Phase 13 — Autopkgtests (Interactive)

- `ppa::get_staging_ppa_test_urls(pkg_name, release)` — runs
  `ppa tests ppa:rust-toolchain/staging -p rustc-<X.Y> --release <release> --show-url`.
- Print all URLs (or a manual fallback message if none returned).
- Confirm all autopkgtests passed (the self-build test was disabled in Phase 7).

### Phase 14 — Push Branch to Foundations Repository (Interactive)

- `git::push_branch(repo_dir, git_remote, target_branch)` (honours `--dry-run`) — the
  branch is pushed only after the staging build and autopkgtests pass, becoming the
  authoritative record of the backport.

### Phase 15 — Archive Upload (Optional, Interactive)

- Print info-box explaining that archive upload is only needed if the backport is
  specifically required in the Archive (the staging PPA is sufficient for bootstrapping).
- List what to include in the Security team request: bug link, staging PPA link,
  package name and version.
- Print link to the Security Proposed PPA for monitoring.
- `prompt_select` to finish.

---

## What is NOT Automated

The "Common Backporting Changes" (LLVM vendoring, libgit2 vendoring, pkgconf → pkg-config,
cmake-mozilla, dh-cargo removal, debhelper-compat downgrade, RISC-V `zicsr`/`zmmul`
patches, stage0 bootstrap) are situation-specific and require human judgment.

Phase 8 references the documentation and lists common issues, but does not apply these
changes automatically.  This is the same philosophy as `update`'s Phase 9 (Remove
Vendored C Libraries), which guides but does not automate.

---

## Testing

All new pure functions are unit-tested:

- `UbuntuRelease::series_number()` — verified for focal, jammy, noble, questing.
- `compute_backport_version()` — verified against the two doc examples and an
  already-versioned source → older-release case.
- `strip_series_suffix()` and `is_series_like()` — individual unit tests.
- `disable_self_build_test()` — two cases: block present (removed), block absent (no-op).
- `tarball_fetch::expected_backport_tarball_name()` — both tarball kinds.
- `tarball_fetch::launchpad_files_url()` — `+` percent-encoding, `~` preserved.
- `tarball_fetch::parse_rmadison_upstream_versions()` — matching, series-suffixed,
  non-matching, and malformed rmadison lines.
- `BackportParams::new()` — invalid lpuser, invalid bug number, same source/target release.
- `find_changes_file()` in `backport.rs` — newest-file selection.
