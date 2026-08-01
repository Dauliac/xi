use std::process::{Command, Stdio};

use color_eyre::{Result, eyre::eyre};
use tracing::debug;

use crate::args::AuthArgs;

const NIX_AUTH_BIN: &str = "nix-auth";
const INSTALL_HINT: &str = "install it with `nix profile install \
                            github:numtide/nix-auth`, or ensure the xi \
                            package's runtime PATH includes it";

/// Locate the `nix-auth` binary, resolving through `PATH`.
fn locate_binary() -> Result<std::path::PathBuf> {
  which::which(NIX_AUTH_BIN).map_err(|err| {
    eyre!("could not find `{NIX_AUTH_BIN}` on PATH: {err}\n{INSTALL_HINT}")
  })
}

/// Run `xi auth <subcommand> [args...]` by exec-ing the upstream `nix-auth`
/// binary.
///
/// # Errors
///
/// Returns an error if `nix-auth` cannot be located on `PATH` or if the
/// child process fails to spawn. When `nix-auth` itself exits non-zero,
/// this function exits the current process with the same status code
/// rather than returning.
pub fn run(args: AuthArgs) -> Result<()> {
  let bin = locate_binary()?;
  let subcommand = args.command.subcommand_name();
  let passthrough = args.command.passthrough_args();

  debug!(
    "Running: {} {} {}",
    bin.display(),
    subcommand,
    passthrough.join(" ")
  );

  let status = Command::new(&bin)
    .arg(subcommand)
    .args(passthrough)
    .stdin(Stdio::inherit())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .status()
    .map_err(|err| eyre!("failed to spawn `{NIX_AUTH_BIN}`: {err}"))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  Ok(())
}
