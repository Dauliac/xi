#![allow(dead_code)] // Some code is reserved for future features

mod activate;
pub mod args;
pub mod daemon;
mod diff;
pub mod dirs;
mod enter;
pub mod env_file;
mod exec;
pub mod flake_detect;
mod meta;
pub mod prompt;
pub mod shell;
mod store_path;
pub mod subshell;
mod switch;
pub mod timeouts;
mod trust;
mod ui;

/// Compute a flake ID from a path (public for integration tests).
/// Delegates to `dirs::flake_id`.
#[must_use]
pub fn compute_flake_id(path: &std::path::Path) -> String {
  dirs::flake_id(path)
}

use color_eyre::Result;

use crate::args::{DaemonAction, DevelopAction, DevelopArgs};

impl DevelopArgs {
  /// Run the develop command.
  ///
  /// # Errors
  ///
  /// Returns an error if the command fails.
  pub fn run(self) -> Result<()> {
    match self.action {
      Some(DevelopAction::Trust(ref a)) => {
        let flake = self.resolve_flake();
        let target = a.target.as_deref().unwrap_or("default");
        trust::trust_flake(&flake, target)
      },
      Some(DevelopAction::Untrust(ref a)) => {
        let flake = self.resolve_flake();
        let target = a.target.as_deref().unwrap_or("default");
        trust::untrust_flake(&flake, target)
      },
      Some(DevelopAction::Activate(ref a)) => {
        activate::generate_activation_script(&a.shell)
      },
      Some(DevelopAction::Switch(ref a)) => switch::switch(a),
      Some(DevelopAction::Exec(ref a)) => exec::exec(a),
      Some(DevelopAction::List(ref a)) => list(a),
      Some(DevelopAction::Clean(ref a)) => clean(a),
      Some(DevelopAction::Daemon(ref a)) => run_daemon(a),
      Some(DevelopAction::Prompt(ref a)) => prompt::handle_prompt(a),
      Some(DevelopAction::Status) => status(),
      None => enter::enter(&self),
    }
  }

  fn resolve_flake(&self) -> String {
    self.flake.clone().unwrap_or_else(|| ".".to_string())
  }
}

fn run_daemon(args: &args::DaemonArgs) -> Result<()> {
  match &args.action {
    DaemonAction::Start(a) => {
      let flake_root = std::path::Path::new(&a.flake);
      let canonical = std::fs::canonicalize(flake_root)?;
      let fid = dirs::flake_id(&canonical);
      let state_dir = dirs::state_dir(&fid);
      let socket_path = dirs::daemon_socket_path(&fid);

      dirs::ensure_cache_version(&state_dir)?;

      let eval_cache_mode = daemon::server::EvalCacheMode::parse(&a.eval_cache);
      let state = std::sync::Arc::new(daemon::server::ServerState::new(
        canonical,
        state_dir,
        a.eval_interval,
        a.watch_extra.clone(),
        xi_core::cache_queue::QueueConfig::default(),
        eval_cache_mode,
      ));
      daemon::server::run(&socket_path, &state)
    },
    DaemonAction::Stop(a) => {
      let flake_ref = a.flake.as_deref().unwrap_or(".");
      daemon::lifecycle::stop(std::path::Path::new(flake_ref))
    },
    DaemonAction::Status(a) => {
      let flake_ref = a.flake.as_deref().unwrap_or(".");
      let canonical = std::fs::canonicalize(flake_ref)?;
      let fid = dirs::flake_id(&canonical);
      let socket_path = dirs::daemon_socket_path(&fid);

      match daemon::client::status(&socket_path) {
        Ok(s) => {
          ui::info(format!(
            "daemon {:?} | uptime {}s | target {} | {} packages | {} consumers | v{}",
            s.state,
            s.uptime_secs,
            s.current_target,
            s.package_count,
            s.consumer_count,
            s.version
          ));
        },
        Err(_) => {
          ui::warn("daemon not running");
        },
      }
      Ok(())
    },
  }
}

fn clean(args: &args::CleanArgs) -> Result<()> {
  if args.all {
    let state_base = dirs::state_base();
    if state_base.exists() {
      ui::loading(format!(
        "removing all cached state: {}",
        state_base.display()
      ));
      std::fs::remove_dir_all(&state_base)?;
    }
    ui::success("all devshell caches cleared");
    return Ok(());
  }

  let flake_ref = args.flake.as_deref().unwrap_or(".");
  let fid = dirs::flake_id_from_ref(flake_ref)?;
  let state_dir = dirs::state_dir(&fid);

  if state_dir.exists() {
    ui::loading(format!("removing cached state: {}", state_dir.display()));
    std::fs::remove_dir_all(&state_dir)?;

    // Check if the flake is trusted and the hook is active
    let canonical = std::fs::canonicalize(flake_ref).ok();
    let is_trusted = canonical.as_deref().is_some_and(trust::is_trusted);
    let hook_active = std::env::var_os("__XI_BIN").is_some();

    if is_trusted && hook_active {
      ui::success("devshell cache cleared — will re-evaluate on next prompt");
    } else if is_trusted {
      ui::success(
        "devshell cache cleared — run eval \"$(xi develop activate zsh)\" to enable auto-reload",
      );
    } else {
      ui::success(
        "devshell cache cleared — run \"xi develop trust\" to enable auto-activation",
      );
    }
  } else {
    ui::info("no cached state found for this flake");
  }

  Ok(())
}

fn list(args: &args::ListArgs) -> Result<()> {
  use xi_core::style::{self, Icon, color};
  use yansi::Paint;

  let flake_ref = args.flake.as_deref().unwrap_or(".");
  let fid = dirs::flake_id_from_ref(flake_ref)?;
  let state_dir = dirs::state_dir(&fid);

  let m = meta::load(&state_dir).map_err(|_| {
    color_eyre::eyre::eyre!(
      "No cached devshell. Run 'xi develop' or enter the flake directory first."
    )
  })?;

  if m.packages.is_empty() {
    ui::info("devshell has no packages");
    return Ok(());
  }

  // JSON output
  if args.json {
    let json = serde_json::to_string_pretty(&m.packages)?;
    println!("{json}");
    return Ok(());
  }

  // Sort packages by name
  let mut pkgs = m.packages.clone();
  pkgs.sort_by(|a, b| a.name.cmp(&b.name));

  // Header
  println!(
    "{} {} packages in devshell {}",
    style::colored(Icon::Info.glyph(), color::GREEN),
    style::bold(&pkgs.len().to_string()),
    style::dim(&format!("(target: {})", m.target)),
  );
  println!();

  // Column widths
  let max_name = pkgs.iter().map(|p| p.name.len()).max().unwrap_or(20);
  let name_width = max_name.min(30);

  for pkg in &pkgs {
    let ver = pkg.version.as_deref().unwrap_or("");
    let name_display = if pkg.name.len() > name_width {
      format!("{}…", &pkg.name[..name_width - 1])
    } else {
      pkg.name.clone()
    };

    if args.paths {
      println!(
        "  {} {:<width$}  {}  {}",
        Paint::green("●"),
        Paint::new(&name_display).bold(),
        Paint::new(ver).dim(),
        Paint::new(&pkg.store_path).dim(),
        width = name_width,
      );
    } else {
      println!(
        "  {} {:<width$}  {}",
        Paint::green("●"),
        Paint::new(&name_display).bold(),
        Paint::new(ver).dim(),
        width = name_width,
      );
    }
  }

  Ok(())
}

fn status() -> Result<()> {
  use crate::daemon::protocol::DaemonState;
  use xi_core::style::{self, Icon, color};

  // Current flake context
  let cwd = std::env::current_dir()?;
  let flake_root = find_flake_root(&cwd);

  if let Some(ref root) = flake_root {
    let fid = dirs::flake_id(root);
    let state_dir = dirs::state_dir(&fid);
    let socket_path = dirs::daemon_socket_path(&fid);
    let trusted = trust::is_trusted(root);

    // Header
    println!("{}", style::bold(&root.display().to_string()));
    println!("  flake id: {}", style::dim(&fid));

    // Trust
    if trusted {
      println!("  {} trusted", Icon::Success.render());
    } else {
      println!(
        "  {} untrusted — run {}",
        Icon::Warn.render(),
        style::bold("xi develop trust")
      );
    }

    // Session: detect if we're inside a sync devshell
    let in_devshell =
      std::env::var("IN_NIX_SHELL").ok().as_deref() == Some("impure");
    if in_devshell {
      println!(
        "  session:  {}",
        style::colored(
          &format!("{} active (sync)", Icon::Success.glyph()),
          color::GREEN,
        )
      );
    }

    // Daemon
    if let Ok(s) = daemon::client::status(&socket_path) {
      let state_str = match &s.state {
        DaemonState::Ready => {
          format!(
            "{}{} ready{}",
            color::GREEN,
            Icon::Info.glyph(),
            color::RESET
          )
        },
        DaemonState::Evaluating => {
          format!(
            "{}{} building{}",
            color::BLUE,
            Icon::Loading.glyph(),
            color::RESET
          )
        },
        DaemonState::Starting => {
          format!(
            "{}{} starting{}",
            color::BLUE,
            Icon::Loading.glyph(),
            color::RESET
          )
        },
        DaemonState::BuildFailed { .. } => {
          format!(
            "{}{} error{}",
            color::RED,
            Icon::Error.glyph(),
            color::RESET
          )
        },
        DaemonState::WatcherDegraded => {
          format!(
            "{}{} degraded (watcher){}",
            color::YELLOW,
            Icon::Warn.glyph(),
            color::RESET
          )
        },
        DaemonState::ConfigError { .. } => {
          format!(
            "{}{} config error{}",
            color::RED,
            Icon::Error.glyph(),
            color::RESET
          )
        },
        DaemonState::ShuttingDown => {
          format!(
            "{}{} shutting down{}",
            color::YELLOW,
            Icon::Loading.glyph(),
            color::RESET
          )
        },
        // v3-only variants (Pending, Missing, Degraded, Stuck, SelfHealing)
        // — v3 protocol renderer lives in a sibling task. For now render
        // via the state catalog so this call site stays uniform.
        other => {
          use crate::daemon::state_meta;
          let meta =
            state_meta::meta_for_state(other).expect("state_meta parity");
          format!(
            "{}{} {}{}",
            color::YELLOW,
            Icon::Info.glyph(),
            meta.display,
            color::RESET
          )
        },
      };
      println!(
        "  daemon:   {state_str} (uptime {}s, {} consumers)",
        s.uptime_secs, s.consumer_count
      );
      println!("  target:   {}", style::bold(&s.current_target));
      println!("  packages: {}", s.package_count);

      // Cache push state
      if !s.active_cache_pushes.is_empty() {
        let count = s.active_cache_pushes.len();
        println!(
          "  cache:    {}",
          style::colored(
            &format!(
              "{} pushing {count} path{}",
              Icon::Loading.glyph(),
              if count == 1 { "" } else { "s" }
            ),
            color::BLUE,
          )
        );
        for path in &s.active_cache_pushes {
          tracing::debug!("  cache push: {path}");
        }
      }
    } else {
      let in_devshell = std::env::var_os("__XI_IN_DEVSHELL").is_some();
      let msg = if in_devshell {
        format!(
          "{} stopped (idle) — will restart on next prompt",
          Icon::Info.glyph()
        )
      } else {
        format!(
          "{} not running — will start when entering flake dir",
          Icon::Info.glyph()
        )
      };
      println!("  daemon:   {}", style::colored(&msg, color::YELLOW));
    }

    // Cached env
    if let Ok(m) = meta::load(&state_dir) {
      let age = meta::now_secs().saturating_sub(m.timestamp);
      let age_str = if age < 60 {
        format!("{age}s ago")
      } else if age < 3600 {
        format!("{}m ago", age / 60)
      } else {
        format!("{}h ago", age / 3600)
      };
      #[allow(clippy::cast_precision_loss)]
      let eval_secs = m.eval_duration_ms as f64 / 1000.0;
      println!(
        "  last eval: {} ({eval_secs:.1}s, {} packages)",
        age_str,
        m.packages.len()
      );

      // Store path (derivation output)
      if let Some(ref sp) = m.store_path {
        println!("  drv path: {}", style::dim(sp));
      }

      // Show env file
      let env_link =
        dirs::current_link(&state_dir, &format!("env-{}", m.target), "sh");
      if env_link.exists() || std::fs::read_link(&env_link).is_ok() {
        println!(
          "  env file: {}",
          style::dim(&env_link.display().to_string())
        );
      }

      // Eval cache info
      if let Some(ref ih) = m.input_hash {
        println!(
          "  eval cache: {} ({})",
          style::dim(ih),
          style::dim("lock mode")
        );
      }
    } else {
      println!("  last eval: {}", style::dim("none"));
    }

    // Watched files count (only files matching default + extra patterns)
    if let Ok(repo) = git2::Repository::open(root)
      && let Ok(index) = repo.index()
    {
      let count = index
        .iter()
        .filter(|e| {
          let p = String::from_utf8_lossy(&e.path);
          let filename = p.rsplit('/').next().unwrap_or(&p);
          filename == "flake.nix" || filename == "flake.lock"
        })
        .count();
      println!("  watched:  {count} files (flake.nix + flake.lock)");
    }

    // Cache push queue
    let queue_count = xi_core::cache_queue::pending_count();
    if queue_count > 0 {
      println!(
        "  queue:    {} — run {} to flush",
        style::colored(
          &format!(
            "{} {queue_count} pending push{}",
            Icon::Warn.glyph(),
            if queue_count == 1 { "" } else { "es" }
          ),
          color::YELLOW,
        ),
        style::bold("xi cache retry"),
      );
    }

    // Error details
    if let Ok(s) = daemon::client::status(&socket_path)
      && matches!(s.state, DaemonState::BuildFailed { .. })
    {
      println!();
      println!("  {}", style::colored("Last error:", color::RED));
      let cwd = root.display().to_string();
      if let Ok(prompt_resp) = daemon::client::prompt(
        &socket_path,
        0,
        &s.current_target,
        &cwd,
        false,
        None,
      ) {
        for notif in &prompt_resp.notifications {
          if notif.kind == crate::daemon::protocol::NotifKind::Error {
            for line in notif.message.lines() {
              println!("    {line}");
            }
          }
        }
      }
    }
  } else {
    println!("{}", style::dim("Not in a flake directory"));
  }

  Ok(())
}

/// Walk up to find flake.nix. Delegates to `flake_detect::find_flake_root`.
fn find_flake_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
  flake_detect::find_flake_root(start)
}
