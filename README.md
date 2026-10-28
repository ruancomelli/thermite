<img width="1024" height="559" alt="thermite_logo-small" src="https://github.com/user-attachments/assets/b680da77-5fc6-452c-a24c-5c49d6f9cbf9" />

# thermite

**thermite is beta software.  Please use it with caution and report any bugs you discover.**

thermite is a Ubuntu Linux command-line tool that automates packaging the upstream Rust
toolchain into versioned Ubuntu `.deb` source packages.  It implements the two primary
workflows used by Ubuntu Foundations toolchain maintainers:

- **`update`** — create a new versioned `rustc-X.Y` source package for a new upstream
  Rust release (e.g. `rustc-1.85`).
- **`backport`** — adapt an existing versioned `rustc-X.Y` package for an older Ubuntu
  Long-Term Support (LTS) release.

Reference workflows:
- Update (official docs): <https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/update-rust/>
- Backport (official docs): <https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/>
- Backport (runbook): `docs/rust-backporting-runbook.md` — an AI-generated, Design-by-Contract formalisation of the official backport docs; subordinate to them and kept in sync.

---

## Overview

Every new upstream Rust release requires a new versioned source package (e.g. `rustc-1.85`)
to be created in the Ubuntu archive.  The process involves many ordered steps: fetching the
upstream source with `uscan`, refreshing Debian patches with `quilt`, pruning unwanted
vendored dependencies, updating control files, running local builds with `sbuild`, checking
the result with `lintian`, uploading to a PPA, running `autopkgtest`s, and finally requesting
sponsorship.

thermite drives the entire sequence, fully automating what can be automated and pausing at
checkpoints that require human judgment (patch conflicts, copyright stanza authoring, build
failure triage, etc.).

---

## Prerequisites

The following tools must be installed and available on `PATH` before running thermite.
On Ubuntu, most are available through the standard package repositories or as snaps.

| Tool | Ubuntu package / snap | Purpose |
|------|-----------------------|---------|
| `git` | `git` | Version control |
| `dch` | `devscripts` | Debian changelog editing |
| `uscan` | `devscripts` | Upstream source download |
| `gbp` | `git-buildpackage` | Import upstream source into Git |
| `quilt` | `quilt` | Patch management |
| `dpkg-buildpackage` | `dpkg-dev` | Source package building |
| `sbuild` | `sbuild` | Clean-room binary package builds |
| `lintian` | `lintian` | Debian policy compliance checks |
| `cargo` | `rustup` snap | Rust build tool (for vendor pruning) |
| `rustup` | `rustup` snap | Rust toolchain manager |
| `dput` | `dput` | Upload packages to Launchpad PPAs |
| `ppa` | `ppa-dev-tools` snap | Create and manage Launchpad PPAs |
| `autopkgtest` | `autopkgtest` | Installed-package test runner |
| `autopkgtest-buildvm-ubuntu-cloud` | `autopkgtest` | Create QEMU autopkgtest images |

Install the snaps:

```sh
sudo snap install rustup --classic
sudo snap install ppa-dev-tools
```

Install the apt packages:

```sh
sudo apt install git devscripts git-buildpackage quilt dpkg-dev \
    sbuild lintian dput autopkgtest
```

---

## Installation

thermite is written in Rust and built with Cargo.

```sh
git clone https://github.com/your-org/thermite.git
cd thermite
cargo build --release
# Optionally install to ~/.cargo/bin
cargo install --path . --features tui
```

The `thermite tui` interactive terminal UI is feature-gated: pass
`--features tui` to `cargo build`/`cargo install` to include it (this pulls in
the ratatui and crossterm dependencies).

---

## Usage

```
thermite [OPTIONS] <COMMAND>

Options:
  -v, --verbose    Pass once to print each external command before it runs.
                   Pass twice (-vv) to also show a concise explanation with
                   documentation links at the start of every phase.
  -h, --help       Print help
  -V, --version    Print version

Commands:
  update    Package a new upstream Rust toolchain release for Ubuntu
  backport  Backport an existing Rust toolchain package to an older Ubuntu release
  version   Parse, explain, format, or bump Ubuntu rustc package version strings
  tarball   Download or regenerate the orig and orig-vendor source tarballs
  tui       Interactive terminal UI over the packaging commands (feature-gated)
  help      Print this message or the help of the given subcommand(s)
```

### `thermite update`

Creates a new versioned `rustc-X.Y` Ubuntu source package for a new upstream Rust release.

```
thermite update [OPTIONS]
  -u, --rust-update-version <X.Y.Z>   Rust version being packaged  (e.g. 1.85.1)
  -o, --rust-old-version    <X.Y.Z>   Rust version being replaced  (e.g. 1.84.0)
  -r, --release             <NAME>    Target Ubuntu release         (e.g. noble)
  -l, --lpuser              <NAME>    Launchpad username
  -b, --lp-bug-number       <NUMBER>  Launchpad bug ID for this work
  -g, --git-remote          <NAME>    Local remote for Foundations rustc repo
                                      [default: foundations]
  -d, --repo-dir            <PATH>    Debian source package root
                                      [default: current directory]
```

#### Example: update Rust from 1.84.0 to 1.85.1 for Noble

```sh
cd ~/rustc/rustc          # the cloned Foundations rustc Git repository
thermite update \
  --rust-update-version 1.85.1 \
  --rust-old-version    1.84.0 \
  --release             noble \
  --lpuser              jdoe \
  --lp-bug-number       2109761
```

#### Example: same update with verbose command output

```sh
thermite -v update \
  --rust-update-version 1.85.1 \
  --rust-old-version    1.84.0 \
  --release             noble \
  --lpuser              jdoe \
  --lp-bug-number       2109761
```

The `-v` flag prints every external command (prefixed with `+`) before it is
executed, which is useful for understanding exactly what thermite is doing or for
diagnosing failures.

#### Example: update with per-phase explanations

```sh
thermite -vv update \
  --rust-update-version 1.85.1 \
  --rust-old-version    1.84.0 \
  --release             noble \
  --lpuser              jdoe \
  --lp-bug-number       2109761
```

The `-vv` flag enables everything `-v` does and additionally shows a concise
explanation with a link to the official documentation at the start of every phase.
This is particularly useful for maintainers who are new to the Rust toolchain
packaging workflow.

### `thermite backport`

Adapts an existing versioned `rustc-X.Y` package for an older Ubuntu release.

```
thermite backport [OPTIONS]
  -u, --rust-version      <X.Y.Z>   Rust version to backport        (e.g. 1.85.0)
  -s, --source-release    <NAME>    Ubuntu release to port FROM      (e.g. noble, or 'devel')
  -r, --release           <NAME>    Ubuntu release to port TO        (e.g. jammy)
  -l, --lpuser            <NAME>    Launchpad username
  -b, --lp-bug-number     <NUMBER>  Launchpad bug ID (optional; omit for proactive backports)
  -g, --git-remote        <NAME>    Local remote for Foundations rustc repo
                                    [default: foundations]
  -d, --repo-dir          <PATH>    Debian source package root
                                    [default: current directory]
```

#### Example: backport Rust 1.85.0 from Noble to Jammy (with bug)

```sh
cd ~/rustc/rustc
thermite backport \
  --rust-version    1.85.0 \
  --source-release  noble \
  --release         jammy \
  --lpuser          jdoe \
  --lp-bug-number   2100492
```

#### Example: proactive backport (no bug number)

```sh
thermite backport \
  --rust-version    1.85.0 \
  --source-release  noble \
  --release         jammy \
  --lpuser          jdoe
```

#### Example: backport with per-phase explanations (recommended for first-time use)

```sh
thermite -vv backport \
  --rust-version    1.85.0 \
  --source-release  noble \
  --release         jammy \
  --lpuser          jdoe
```

Pass `-vv` to display a concise explanation of what each phase does and why,
together with a link to the relevant section of the official docs.
Pass `-v` (single) to print each external command without the explanations.

### `thermite version`

Utilities for working with Ubuntu `rustc` package version strings.  The version
string format is documented in the [Ubuntu project docs](https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/rust-version-strings/).

```
thermite version <SUBCOMMAND>

Subcommands:
  parse    Parse a version string and display its components
  explain  Explain each component of a version string with documentation
  format   Construct a version string from individual components
  bump     Bump a version string according to a specified operation
```

#### `thermite version parse`

Parse a version string into its structured components:

```sh
thermite version parse "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1"
# Upstream version:  1.90.0
# Repack number:     2
# Series:            24.04
# ...

thermite version parse "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1" --json
# {"upstream":{"major":1,"minor":90,"patch":0},"repack_number":2,...}

# Or read from debian/changelog:
thermite version parse --from-changelog
```

#### `thermite version explain`

Display an educational breakdown of each component:

```sh
thermite version explain "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1"
```

#### `thermite version format`

Construct a version string from individual options:

```sh
thermite version format --upstream 1.95.0 --release noble --ubuntu-revision 3
# 1.95.0+dfsg~24.04-0ubuntu3~24.04.1

thermite version format --upstream 1.95.0
# 1.95.0+dfsg-0ubuntu1
```

#### `thermite version bump`

Apply a bump operation to a version string:

```sh
# Increment Ubuntu revision
thermite version bump "1.95.0+dfsg-0ubuntu1" ubuntu-revision
# 1.95.0+dfsg-0ubuntu2

# Generate a backport version
thermite version bump "1.95.0+dfsg-0ubuntu2" backport --release noble
# 1.95.0+dfsg~24.04-0ubuntu2~24.04.1

# Retarget to a different series
thermite version bump "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1" retarget --series 22.04
# 1.90.0+dfsg2~22.04-0ubuntu3~22.04.1

# Increment PPA number
thermite version bump "1.95.0+dfsg-0ubuntu1" ppa
# 1.95.0+dfsg-0ubuntu1~ppa1

# Read from changelog:
thermite version bump --from-changelog ubuntu-revision
```

### `thermite tarball`

Standalone management of the two source tarballs outside the full update/backport
workflows. Each tarball can either be **downloaded** (staging PPA first, then the
Ubuntu primary archive via `rmadison`, with a manual-placement fallback) or
**generated** (`uscan` for the orig tarball; `rustup` + `cargo-vendor-filterer` +
`debian/rules vendor-tarball` for the vendor tarball). An already-obtained
tarball can also be **overlaid** into the repo directory.

```
thermite tarball download orig|vendor|all [OPTIONS]
thermite tarball generate orig|vendor|all [OPTIONS]
thermite tarball overlay   orig|vendor|all [OPTIONS]

  -u, --rust-version   <X.Y.Z>  Full Rust version the tarballs are named after
  --series             <NAME>   Ubuntu release adjective for backport-style names
                                (e.g. noble → '+dfsg~26.04'). Omit for plain
                                update naming ('+dfsg')
  -d, --repo-dir       <PATH>   Debian source package root
                                [default: current directory]

generate only:
  --force                       Overwrite a tarball that already exists in the
                                parent directory (without it, generate refuses)

vendor and all only (overlay):
  --replace                     Remove the existing vendor/ directory before
                                extracting (clean replace instead of merge)
```

Downloads are idempotent: an existing tarball is reused. Generation overwrites
only with `--force`. The vendor tarball generation requires `rustup` and produces
`../rustc-<X.Y>_<X.Y.Z>+dfsg[~<series>].orig-vendor.tar.xz`. Neither download
nor generate touches the working tree — run `thermite tarball overlay`
afterwards to extract a tarball into the repo dir.

Overlay never fetches or produces tarballs: the expected tarball must already
exist in the parent directory (use download/generate first), otherwise the
command refuses. Overlaying the orig tarball extracts its full contents into
the repo dir with the top-level `rustc-<X.Y.Z>-src/` directory stripped;
overlaying the vendor tarball merges `vendor/` into the repo dir, or cleanly
replaces it with `--replace`. With `all`, the orig tarball is overlaid first
and `--replace` applies to the vendor part only.

#### Example: regenerate the orig tarball for a backport

```sh
cd ~/rustc/rustc
thermite tarball generate orig \
  --rust-version 1.85.0 \
  --series       noble
# produces ../rustc-1.85_1.85.0+dfsg~26.04.orig.tar.xz
```

#### Example: regenerate the vendor tarball and overlay it (clean replace)

```sh
thermite tarball generate vendor \
  --rust-version 1.85.0 \
  --series       noble \
  --force
thermite tarball overlay vendor \
  --rust-version 1.85.0 \
  --series       noble \
  --replace
```

#### Example: download both tarballs for a plain update naming scheme

```sh
thermite tarball download all --rust-version 1.85.1
```

#### Example: restore the working tree from existing tarballs after a git clean

```sh
cd ~/rustc/rustc
thermite tarball overlay all \
  --rust-version 1.85.0 \
  --series       noble
```

### `thermite tui`

An interactive terminal UI (built with ratatui) that exposes the standalone
packaging commands as keyboard shortcuts over the current repo directory.
Because it is behind a feature flag, it requires building thermite with
`--features tui` (or installing from a source build with that feature
enabled).

```sh
thermite tui [--repo-dir <DIR>]
```

| Key | Action |
|-----|--------|
| `b` | Build with `sbuild` (prompts for the Ubuntu release adjective, e.g. noble) |
| `S` | Source build with `dpkg-buildpackage -S -I -i -nc -d -sa` |
| `l` | Run `lintian` on the repo directory |
| `v` | Open the version bump menu (bump upstream / series / patch / dfsg / ubuntu rev …) |
| `V` | Set the changelog version to an explicit string |
| `t` | Open the tarball menu (download / generate / overlay × orig / vendor / all) |
| `g` | `git fetch --all` |
| `R` | Refresh the repo info panel (branch, version, tarballs, etc.) |
| `q` / Ctrl+C | Quit (disabled while a command is running) |

The command output of each action is captured and shown in a scrollable log
pane (PageUp/PageDown), so several commands can be inspected in order after
they finish. The same `git fetch`, `sbuild`, `lintian`, `tarball`, and version
machinery as the CLI subcommands is reused; only the invocation surface
differs.

---

## Workflow Phases (update command)

thermite runs the following phases in sequence.  Fully automated phases execute without
user input.  Interactive phases print instructions and wait for a keypress before
continuing.  Pass `-vv` to see a concise explanation with documentation links at the
start of each phase.

| Phase | Description | Mode |
|-------|-------------|------|
| 0 | Preflight checks — verify required tools and repository layout | Automated |
| 1 | Create a Launchpad bug report | Interactive |
| 2 | Set up Git branch (`merge-X.Y`) | Automated |
| 3 | Update `debian/changelog`, package name, and `debian/watch` | Automated |
| 4 | Temporarily include all vendored dependencies in `debian/copyright` | Automated |
| 5 | Download upstream source with `uscan` (first pass, full vendor) | Automated |
| 6 | Import upstream source into Git with `gbp import-orig` (first pass) | Automated |
| 7 | Refresh patches with `quilt` | Interactive |
| 8 | Prune unwanted vendored dependencies with `cargo-vendor-filterer` | Automated |
| 9 | Remove vendored C libraries | Interactive |
| 10 | Download upstream source again (second pass, pruned vendor) | Automated |
| 11 | Update versioned package references in control files | Automated |
| 12 | After-repack patch refreshes | Interactive |
| 13 | Update `XS-Vendored-Sources-Rust` in `debian/control` | Automated |
| 14 | Update vendored copyright overrides | Automated |
| 15 | Update `debian/copyright` stanzas | Interactive |
| 16 | Local build and bug fixing with `sbuild` | Interactive |
| 17 | Lintian checks | Interactive |
| 18 | PPA build — upload to Launchpad PPA and verify | Interactive |
| 19 | Run `autopkgtest`s | Interactive |
| 20 | Upload and request sponsorship | Interactive |

---

## Workflow Phases (backport command)

thermite runs the following phases in sequence.  Fully automated phases execute without
user input.  Interactive phases print instructions and wait for a keypress before
continuing.  Pass `-vv` to see a concise explanation with documentation links at the
start of each phase.

| Phase | Description | Mode |
|-------|-------------|------|
| 0 | Preflight checks — verify required tools and repository layout | Automated |
| 1 | Create a Launchpad bug report (optional for proactive backports) | Interactive |
| 2 | Set up Git branch (`<release>-X.Y` from `<source_release>-X.Y`, or `merge-X.Y` when the source is the devel release) | Automated |
| 3 | Compute and apply backport version string; update `debian/changelog` | Automated |
| 4 | Compatibility checks — LLVM, libgit2, dh-cargo, pkgconf, cmake, debhelper-compat against the target release | Interactive |
| 5 | Provide orig tarball — reuse locally, download automatically from the staging PPA / Ubuntu archive, or regenerate with `uscan` | Interactive |
| 6 | Provide orig-vendor tarball — reuse, download, or regenerate with `debian/rules vendor-tarball` | Interactive |
| 7 | Disable autopkgtest self-build test in `debian/tests/control` | Automated |
| 8 | Local build and bug fixing with `sbuild` (skippable) | Interactive |
| 9 | Build source package with `dpkg-buildpackage -S` (skippable if `.dsc` exists) | Interactive |
| 10 | Lintian checks on the source package (skippable) | Interactive |
| 11 | PPA build — upload to personal Launchpad PPA and verify all architectures | Interactive |
| 12 | Staging PPA upload — update changelog and upload to `ppa:rust-toolchain/staging` | Interactive |
| 13 | Autopkgtests — trigger and verify via `ppa tests` on the staging PPA | Interactive |
| 14 | Push branch to the Foundations rustc repository | Interactive |
| 15 | Archive upload (optional) — request via the Ubuntu Security team | Interactive |

---

## Project Structure

```
src/
  bin/thermite.rs       CLI entry point (clap argument parsing and dispatch)
  commands/
    update.rs           Orchestrates the full update workflow
    backport.rs         Orchestrates the full backport workflow
    version.rs          Version string utilities (parse, explain, format, bump)
    tarball.rs          Download/generate/overlay the source tarballs
    tui/                Interactive terminal UI (feature-gated; ratatui)
      mod.rs            Terminal setup/teardown and the event loop
      app.rs            UI state, key handling, and command resolution
      render.rs         ratatui rendering (panes, modals, status bar)
  steps/
    autopkgtest.rs      autopkgtest invocations
    build.rs            dpkg-buildpackage, sbuild, and debian/tests/control editing
    changelog.rs        dch and debian/changelog editing (update and backport)
    control.rs          debian/control and debian/control.in editing
    copyright.rs        debian/copyright editing
    gbp.rs              gbp import-orig
    git.rs              Git operations
    lintian.rs          lintian invocations
    overlay.rs          Extracting tarball contents into the working tree
    patches.rs          quilt push/refresh
    ppa.rs              ppa-dev-tools, dput, and staging PPA helpers
    uscan.rs            uscan and orig tarball management
    vendor.rs           cargo-vendor-filterer and vendor-tarball rule
  types/
    package_version.rs  RustcPackageVersion — full Debian version string parser and formatter
    params.rs           UpdateParams and BackportParams — validated CLI parameters
    ubuntu.rs           UbuntuRelease — validated Ubuntu release names and series numbers
    versions.rs         RustVersion — X.Y.Z and X.Y version newtypes
  error.rs              Unified ThermiteError type (thiserror)
  shell.rs              Async external command runner with streaming output
  ui.rs                 Terminal output helpers (phase headers, prompts, countdown)
  lib.rs                Crate root
```

---

## AI Disclosure

> **Important:** This project was planned, generated, and iteratively refined with the
> assistance of Large Language Model (LLM) tools, specifically GitHub Copilot (powered by
> Claude Sonnet).

LLM assistance was used throughout the development of thermite, including:

- **Planning** — the implementation plans in `/plans/` were drafted with LLM assistance
  based on the official Ubuntu Rust packaging docs.
- **Code generation** — the majority of the Rust source code was generated or substantially
  revised through LLM-assisted sessions in VS Code.
- **Bug fixing** — several correctness issues (error handling, version substitution in
  `debian/watch`, rate-limit retry logic, etc.) were identified and resolved through
  LLM-assisted review.
- **Documentation** — this README and inline doc comments were written with LLM assistance.

All generated code and documentation has been reviewed by human maintainers, but users
should be aware that LLM-generated code can contain subtle errors.  Before running
thermite against a real Ubuntu package repository, review the workflow phases carefully
and verify that the tool's actions match the official docs linked above.

If you find a bug or an incorrect workflow step, please open an issue.

---

## License

See [LICENSE](LICENSE).
