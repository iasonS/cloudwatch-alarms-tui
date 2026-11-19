//! opsmon: the CloudWatch alarms of an AWS account in the terminal, in the look of macmon. It only
//! reads, through the `aws` CLI with a read-only profile.

mod alarms;
mod aws;
mod ui;

use std::error::Error;
use std::io::{self, Stdout, Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use alarms::Alarm;
use aws::Aws;
use chrono::Utc;
use clap::Parser;
use ratatui::crossterm::event::{self, KeyEvent};
use ratatui::crossterm::{ExecutableCommand, cursor, terminal};
use ratatui::prelude::*;
use ui::{Action, App, History};

type WithError<T> = Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(version, about = "CloudWatch alarms in the terminal, read with a read-only AWS profile")]
struct Args {
  /// AWS CLI profile to read with; without it the CLI picks its own
  #[arg(long)]
  profile: Option<String>,
  /// Region of the alarms
  #[arg(long)]
  region: Option<String>,
  /// SSO session named in the hint when the login has expired
  #[arg(long)]
  sso_session: Option<String>,
  /// Start of alarm names left out on screen, such as `prod-`
  #[arg(long, default_value = "")]
  prefix: String,
  /// Seconds between refreshes
  #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(10..))]
  interval: u64,
  /// Fetch once, print the screen as text at this size and exit
  #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = parse_size)]
  print: Option<(u16, u16)>,
}

fn parse_size(size: &str) -> Result<(u16, u16), String> {
  let wrong = || format!("{size:?} is not WIDTHxHEIGHT, such as 140x45");
  let (width, height) = size.split_once('x').ok_or_else(wrong)?;
  Ok((width.parse().map_err(|_| wrong())?, height.parse().map_err(|_| wrong())?))
}

fn main() -> WithError<()> {
  let args = Args::parse();
  let aws = Aws {
    profile: args.profile.clone(),
    region: args.region.clone(),
    sso_session: args.sso_session,
  };
  let profile = args.profile.unwrap_or_else(|| "default profile".to_string());
  let region = args.region.unwrap_or_else(|| "default region".to_string());
  let app = App::new(profile, region, args.prefix);
  match args.print {
    Some((width, height)) => print_screen(app, &aws, width, height),
    None => run(app, aws, Duration::from_secs(args.interval)),
  }
}

// MARK: Fetching

enum Event {
  Key(KeyEvent),
  Resize,
  Fetching,
  Alarms(Result<Vec<Alarm>, String>),
  History(Result<History, String>),
}

/// Fetches on the first request, then every `interval` and on each request after it; a request
/// is the graph window in seconds. Requests that come during a fetch make one more fetch.
fn run_fetch_thread(aws: Aws, tx: Sender<Event>, requests: Receiver<i64>, interval: Duration) {
  thread::spawn(move || {
    let Ok(mut window) = requests.recv() else { return };
    while fetch(&aws, &tx, window) {
      match requests.recv_timeout(interval) {
        Ok(next) => window = next,
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => return,
      }
      while let Ok(next) = requests.try_recv() {
        window = next;
      }
    }
  });
}

/// The alarms, then the datapoints of their metrics over the last `window` seconds, as events.
/// False once the app is gone.
fn fetch(aws: &Aws, tx: &Sender<Event>, window: i64) -> bool {
  if tx.send(Event::Fetching).is_err() {
    return false;
  }
  let alarms = aws.alarms();
  let fetched = alarms.as_ref().ok().cloned();
  if tx.send(Event::Alarms(alarms)).is_err() {
    return false;
  }
  match fetched {
    Some(alarms) => tx.send(Event::History(history(aws, &alarms, window))).is_ok(),
    None => true,
  }
}

/// The last `window` seconds of the alarms' metrics, up to the last whole minute.
fn history(aws: &Aws, alarms: &[Alarm], window: i64) -> Result<History, String> {
  let end = Utc::now().timestamp() / 60 * 60;
  let start = end - window;
  aws.history(alarms, start, end).map(|(points, error)| History { start, end, points, error })
}

fn print_screen(mut app: App, aws: &Aws, width: u16, height: u16) -> WithError<()> {
  let alarms = aws.alarms();
  let fetched = alarms.as_ref().ok().cloned();
  app.set_alarms(alarms, Utc::now().timestamp());
  if let Some(alarms) = fetched {
    app.set_history(history(aws, &alarms, app.window_seconds()));
  }
  print!("{}", app.screen(width, height, Utc::now().timestamp()));
  Ok(())
}

// MARK: Terminal

fn run(mut app: App, aws: Aws, interval: Duration) -> WithError<()> {
  app.three_levels = ui::theme::detect_three_level_bars();
  let (tx, rx) = mpsc::channel();
  let (requests, incoming) = mpsc::channel();
  requests.send(app.window_seconds())?;
  run_fetch_thread(aws, tx.clone(), incoming, interval);

  // the guard restores the terminal on every way out of here, `?` included
  let (mut term, _guard) = enter_term()?;
  run_inputs_thread(tx);
  loop {
    term.draw(|f| app.render(f, Utc::now().timestamp()))?;
    match rx.recv()? {
      Event::Key(key) => match app.handle_key(key) {
        Action::Quit => return Ok(()),
        Action::Refresh => requests.send(app.window_seconds())?,
        Action::None => {}
      },
      Event::Resize => {}
      Event::Fetching => app.fetching(),
      Event::Alarms(result) => app.set_alarms(result, Utc::now().timestamp()),
      Event::History(result) => app.set_history(result),
    }
  }
}

/// Sends keys and resizes; stops once the app is gone.
fn run_inputs_thread(tx: Sender<Event>) {
  thread::spawn(move || {
    while let Ok(event) = event::read() {
      let event = match event {
        event::Event::Key(key) => Event::Key(key),
        event::Event::Resize(..) => Event::Resize,
        _ => continue,
      };
      if tx.send(event).is_err() {
        return;
      }
    }
  });
}

/// Set while the terminal is in raw mode on the alternate screen.
static TERM_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped, so an error return can't leave it in raw mode.
struct TermGuard;

impl Drop for TermGuard {
  fn drop(&mut self) {
    leave_term();
  }
}

/// Raw mode and the alternate screen; whatever happens next, the terminal is restored by the
/// guard, the panic hook or `leave_term`.
fn enter_term() -> WithError<(Terminal<CrosstermBackend<Stdout>>, TermGuard)> {
  std::panic::set_hook(Box::new(|info| {
    leave_term();
    eprintln!("{info}");
  }));

  // set before the first change, so a failure half-way still undoes what was done
  TERM_ACTIVE.store(true, Ordering::SeqCst);
  let guard = TermGuard;
  terminal::enable_raw_mode()?;
  stdout().execute(terminal::EnterAlternateScreen)?;
  Ok((Terminal::new(CrosstermBackend::new(stdout()))?, guard))
}

/// Restores the terminal once, whichever comes first: the normal exit, an error return or a panic.
fn leave_term() {
  restore_term_once(&TERM_ACTIVE, &mut stdout(), terminal::disable_raw_mode);
}

/// Leaves the alternate screen, shows the cursor and turns raw mode off when `active` is set, and
/// clears it. Every step runs even if an earlier one fails. Returns whether it ran.
fn restore_term_once(
  active: &AtomicBool,
  out: &mut impl Write,
  disable_raw_mode: impl FnOnce() -> io::Result<()>,
) -> bool {
  if !active.swap(false, Ordering::SeqCst) {
    return false;
  }

  let _ = out.execute(terminal::LeaveAlternateScreen);
  let _ = out.execute(cursor::Show);
  let _ = disable_raw_mode();
  true
}

#[cfg(test)]
mod tests {
  use std::io;
  use std::sync::atomic::AtomicBool;

  use super::{parse_size, restore_term_once};

  #[test]
  fn restoring_the_terminal_runs_once() {
    let active = AtomicBool::new(true);
    let mut out = vec![];
    let mut calls = 0;
    assert!(restore_term_once(&active, &mut out, || {
      calls += 1;
      Ok(())
    }));
    assert!(!restore_term_once(&active, &mut out, || Err(io::Error::other("again"))));
    assert_eq!(calls, 1);
    assert!(!out.is_empty());
  }

  #[test]
  fn sizes_parse_as_width_x_height() {
    assert_eq!(parse_size("140x45"), Ok((140, 45)));
    assert!(parse_size("140").is_err());
    assert!(parse_size("ax45").is_err());
  }
}
