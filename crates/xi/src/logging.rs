use clap_verbosity_flag::InfoLevel;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
  EnvFilter,
  filter::LevelFilter,
  fmt::{self, FormatEvent, FormatFields},
  prelude::*,
  registry::LookupSpan,
};
use yansi::{Color, Paint};

use crate::Result;

struct InfoFormatter {
  show_timestamp: bool,
}

impl<S, N> FormatEvent<S, N> for InfoFormatter
where
  S: Subscriber + for<'a> LookupSpan<'a>,
  N: for<'a> FormatFields<'a> + 'static,
{
  fn format_event(
    &self,
    ctx: &fmt::FmtContext<'_, S, N>,
    mut writer: fmt::format::Writer,
    event: &Event,
  ) -> std::fmt::Result {
    let metadata = event.metadata();
    let level = metadata.level();

    // Timestamp for debug/trace output
    if self.show_timestamp && *level != Level::INFO {
      let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
      let secs = now.as_secs() % 86400; // time of day
      let h = secs / 3600;
      let m = (secs % 3600) / 60;
      let s = secs % 60;
      let ms = now.subsec_millis();
      write!(
        writer,
        "{} ",
        Paint::new(format!("{h:02}:{m:02}:{s:02}.{ms:03}")).fg(Color::Fixed(8))
      )?;
    }

    match *level {
      Level::ERROR => {
        write!(writer, "{} ", Paint::new("ERROR").fg(Color::Red))?;
      },
      Level::WARN => write!(writer, "{} ", Paint::new("!").fg(Color::Yellow))?,
      Level::INFO => write!(writer, "{} ", Paint::new(">").fg(Color::Green))?,
      Level::DEBUG => {
        write!(writer, "{} ", Paint::new("DEBUG").fg(Color::Blue))?;
      },
      Level::TRACE => {
        write!(writer, "{} ", Paint::new("TRACE").fg(Color::Cyan))?;
      },
    }

    ctx.field_format().format_fields(writer.by_ref(), event)?;

    if *level != Level::INFO
      && let (Some(file), Some(line)) = (metadata.file(), metadata.line())
    {
      write!(writer, " (xi/{file}:{line})")?;
    }

    writeln!(writer)?;
    Ok(())
  }
}

/// Configure error reporting and tracing output.
///
/// # Errors
///
/// Returns an error if installing the error hook fails or if tracing filter
/// directives cannot be parsed.
pub fn setup_logging(
  verbosity: clap_verbosity_flag::Verbosity<InfoLevel>,
) -> Result<()> {
  color_eyre::config::HookBuilder::default()
    .display_location_section(true)
    .panic_section(
      "Please report the bug at https://github.com/Dauliac/xi/issues",
    )
    .display_env_section(false)
    .install()?;

  let fallback_level =
    verbosity
      .log_level()
      .map_or(LevelFilter::WARN, |level| match level {
        clap_verbosity_flag::log::Level::Error => LevelFilter::ERROR,
        clap_verbosity_flag::log::Level::Warn => LevelFilter::WARN,
        clap_verbosity_flag::log::Level::Info => LevelFilter::INFO,
        clap_verbosity_flag::log::Level::Debug => LevelFilter::DEBUG,
        clap_verbosity_flag::log::Level::Trace => LevelFilter::TRACE,
      });

  // When XI_LOG is set, the user controls the filter entirely.
  // When not set, use the verbosity flag's level as the default.
  let env_filter = if std::env::var("XI_LOG").is_ok() {
    EnvFilter::from_env("XI_LOG").add_directive("dix=WARN".parse()?)
  } else {
    EnvFilter::from_env("XI_LOG")
      .add_directive(fallback_level.into())
      .add_directive("dix=WARN".parse()?)
  };

  let is_debug = std::env::var("XI_LOG").is_ok();
  let layer = fmt::layer()
    .with_writer(std::io::stderr)
    .compact()
    .with_line_number(true)
    .event_format(InfoFormatter {
      show_timestamp: is_debug,
    })
    .with_filter(env_filter);

  tracing_subscriber::registry().with(layer).init();

  tracing::trace!("Logging OK");

  Ok(())
}
