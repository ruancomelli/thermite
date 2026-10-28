# Project Overview
thermite is a Ubuntu Linux command-line tool for packaging the upstream Rust toolchain into Ubuntu .deb packages.  thermite is written in async Rust and uses Ubuntu's Debian-style command-line tools to prepare, package, and test a Rust toolchain Ubuntu packge.  The Rust toolchain development guide that describes how the upstream project is built and tested is found at https://rustc-dev-guide.rust-lang.org/.  thermite has two major modes of operation: 'update' which is used to package a Rust toolchain release that has not been packaged before, and 'backport' which is used to adapt an existing Rust toolchain Ubuntu packge for an older Ubuntu Long-Term Support (LTS) release.  The primary reference for the 'update' workflow is https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/update-rust/.  The primary reference for the 'backport' workflow is https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/.  The `thermite` command-line tool has two commands, 'update' and 'backport' which specify the major mode of operation.  Each command requires several parameters which are specified on the command line using flags.  Both long style and short style command line flags are defined for each parameters.  For example, the "release" parameter would have a long style flag "--release" and a short style flag "-r".

## Update Mode Parameters
The 'update' command requires the following parameters:
- 'rust-update-version':  The long format version of the Rust version you are updating to, given in "X.Y.Z" format.  Example:  "1.85.1".
- 'rust-update-version-short':  The short format version of the Rust version you are updating to, given in "X.Y" format.  Example: "1.85".  Note that this parameter is not a command line parameter, but is derived from the 'rust-update-version' by truncating the ".Z" part of the long format.
- 'rust-old-version':  The long format version of the Rust version you are updating from, given in "X.Y.Z" format.  Example:  "1.84.0".
- 'rust-old-version-short':  The short format version of the Rust version you are updating from, given in "X.Y" format.  Example: "1.84".  Note that this parameter is not a command line parameter, but is derived from the 'rust-old-version' by truncating the ".Z" part of the long format.
- 'release':  The target Ubuntu release, specified by the first part of Ubuntu release's name.  Examples: "noble" or "jammy".  This parameter must specify a valid Ubuntu release.
- 'lpuser':  Your Launchpad username. This is also used to refer to your personal Launchpad Git repository’s remote name.
- 'git-remote':  Your local Git remote name for the Foundations rustc Git repository.  This command line parameter is optional and defaults to 'foundations' if not provided on the command line.
- 'lp-bug-number':  The Launchpad bug ID number for this work.  This will be used in the package changelog entry and Git commit comments.
These parameters are used as described in the 'update' workflow:  https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/update-rust/

## Backport Mode Parameters
The 'backport' command requires the following parameters:
- 'rust-backport-version':  The long format version of the Rust version you are backporting, given in "X.Y.Z" format.  Example:  "1.82.0".
- 'rust-backport-version-short':  The short format version of the Rust version you are backporting, given in "X.Y" format.  Example: "1.82".  Note that this parameter is not a command line parameter, but is derived from the 'rust-backport-version' by truncating the ".Z" part of the long format.
- 'rust-old-version':  The long format version of the Rust version before the backport version, given in "X.Y.Z" format.  Example:  "1.81.0".
- 'rust-old-version-short':  The short format version of the Rust version before the backport version, given in "X.Y" format.  Example: "1.81".  Note that this parameter is not a command line parameter, but is derived from the 'rust-old-version' by truncating the ".Z" part of the long format.
- 'release':  The target Ubuntu release, specified by the first part of Ubuntu release's name.  Examples: "resolute" or "noble".  This parameter must specify a valid Ubuntu LTS release.
- 'source-release':  The Ubuntu release that should be used as a starting point for this backport.  Examples: "noble" or "jammy".  This parameter must specify a valid Ubuntu release and cannot be the same as the 'release' parameter.
- 'release-version':  The Ubuntu release version of the taret Ubuntu release.  Example: "22.04".  Note that this parameter is not a command line parameter, but is derived from the 'release' parameter using a mapping from Ubuntu release names to Ubuntu release versions.  Since the 'release' parameter only allows Ubuntu LTS releases, the mapping should only include Ubuntu LTS releases.  This parameter is referred to as 'release_number' in the documentation.
- 'lpuser':  Your Launchpad username. This is also used to refer to your personal Launchpad Git repository’s remote name.
- 'lp-bug-number':  The Launchpad bug ID number for this work.  This will be used in the package changelog entry and Git commit comments.

For example, if you were backporting the 'rustc-1.82' to 'jammy', these parameters would be the following:
- 'rust-backport-version': "1.82.0"
- 'rust-backport-version-short': "1.82"
- 'rust-old-version': "1.81.0"
- 'rust-old-version-short': "1.81"
- 'release': "jammy"
- 'source-release': "noble"
- 'release-number': "22.04"
- 'lpuser': "its-me"
- 'lp-bug-number': "123456789"

These parameters are used as described in the 'backport' workflow: https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/

## Folder Structure
- `/src`: top-level Rust source code folder
- `/src/bin`: binary entry points for the application; the thermite.rs source code for the thermite command-line tool is found here
- `/src/lib.rs`: top-level library for shared modules and functionality
- `/test`: top-level folder for integration tests
- `/plans`: top-level folder containing implementation plans for the 'update' and 'backport' workflows.  These are markdown files that describe the implementation plan for each workflow in detail, including the steps to be taken, the expected inputs and outputs, and any relevant references or documentation.
- `/docs`: top-level folder for documentation related to the project, including design documents, user guides, and reference materials.
- `/examples`: top-level folder for example code and usage demonstrations for the thermite tool.


## Skills
thermite makes heavy use of the `debian-packaging` skill.  This skill provides functionality for working with Debian-style source packages, including preparing source packages, building source packages, and managing package changelogs.  The `debian-packaging` skill is used extensively in both the 'update' and 'backport' workflows to manage the preparation and building of Rust toolchain Ubuntu packages.

thermite also uses the `lpcli` skill to create bug reports and manage interactions with Launchpad.  This skill is used in both the 'update' and 'backport' workflows to create bug reports for the work being done, and to manage interactions with Launchpad throughout the packaging process.

## Coding Standards
- Follow idiomatic Rust practices and community standards as defined in `.github/instructions/rust.instructions.md`.

## Development Checks
Before committing or opening a pull request, run all of the following locally and ensure each succeeds. CI runs the same commands and will fail the build if any of them emit output or exit non-zero.

- **Formatting:** `cargo fmt -- --check` — must produce no output. Run `cargo fmt` (without `--check`) to apply fixes if it reports drift.
- **Lints:** `cargo clippy --locked --all-targets --all-features -- -D warnings` — must exit 0. Treat every warning as an error.
- **Docs:** `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features` — must exit 0. Intra-doc links must resolve and must not point at private items (refer to private items in plain text, not `[`…`]` links).
- **Tests:** `cargo test --locked --all-features` — must exit 0.

## Persona
You are an Ubuntu expert with deep knowledge of Ubuntu releases and packages. You provide guidance on best practices for managing Ubuntu source packages and help troubleshoot issues related to package downloads and release compatibility.

