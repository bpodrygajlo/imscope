/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! `imscope-view`: a low-interactivity, view-only TUI that shows a single
//! scope. Meant to be run as a quick "leave it in a terminal" monitor, or
//! several at once (one per terminal pane/window, each with a different
//! `--scope`) to watch multiple channels from the same producer side by
//! side. Unlike `imscope-tui` it has no editable settings, filters, panes,
//! or mouse handling — just connect, display, and (optionally) switch which
//! plot is shown.

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
};

use imscope::app::{ConnectionState, PlotPane, activate_scope};
use imscope::consumer::{ScopeConfig, WorkerCommand, WorkerEvent, run_worker};
use imscope::tui_render::{self, PlotKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PlotArg {
    Scatter,
    Rms,
    Waveform,
    Histogram,
    Spectrum,
}

impl From<PlotArg> for PlotKind {
    fn from(p: PlotArg) -> PlotKind {
        match p {
            PlotArg::Scatter => PlotKind::Scatter,
            PlotArg::Rms => PlotKind::Rms,
            PlotArg::Waveform => PlotKind::Waveform,
            PlotArg::Histogram => PlotKind::Histogram,
            PlotArg::Spectrum => PlotKind::Spectrum,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Low-interactivity, view-only single-scope TUI for imscope",
    long_about = "Low-interactivity, view-only single-scope TUI for imscope.\n\
                  Connects to one producer and displays one scope. Run several instances \
                  in parallel (e.g. one per terminal pane) with different --scope values \
                  to watch multiple channels from the same producer at once."
)]
struct Args {
    /// Zenoh producer name (the `imscope/<name>/...` key-expression prefix).
    #[arg(long, default_value = "imscope")]
    producer: String,

    /// Optional explicit Zenoh locator to connect to (e.g.
    /// "tcp/127.0.0.1:7447"). If omitted, relies on Zenoh's default
    /// scouting/discovery.
    #[arg(long)]
    zenoh_connect: Option<String>,

    /// Scope to display: exact name, or a unique case-insensitive substring
    /// match. Defaults to the first announced scope. Use --list to see names.
    #[arg(short, long)]
    scope: Option<String>,

    /// Plot to display.
    #[arg(short, long, value_enum, default_value = "waveform")]
    plot: PlotArg,

    /// Accumulate ("stack") successive frames instead of replacing on each
    /// new frame, like the Stacking toggle in imscope-tui/imscope-gui.
    #[arg(long)]
    stack: bool,

    /// Max stacked sample count when --stack is set.
    #[arg(long, default_value_t = 16000)]
    stacking_size: usize,

    /// Connect, print the announced scope names, then exit. Handy for
    /// discovering scope names before launching several `imscope-view`
    /// instances in parallel.
    #[arg(long)]
    list: bool,
}

fn resolve_scope_idx(scopes: &[ScopeConfig], wanted: Option<&str>) -> Result<usize, String> {
    if scopes.is_empty() {
        return Err("Producer announced zero scopes.".to_string());
    }
    let Some(wanted) = wanted else {
        return Ok(0);
    };
    let wanted_lower = wanted.to_lowercase();
    if let Some(idx) = scopes
        .iter()
        .position(|s| s.name.to_lowercase() == wanted_lower)
    {
        return Ok(idx);
    }
    let matches: Vec<usize> = scopes
        .iter()
        .enumerate()
        .filter(|(_, s)| s.name.to_lowercase().contains(&wanted_lower))
        .map(|(i, _)| i)
        .collect();
    let all_names = || {
        scopes
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match matches.as_slice() {
        [idx] => Ok(*idx),
        [] => Err(format!(
            "No scope matching '{}'. Available: {}",
            wanted,
            all_names()
        )),
        _ => Err(format!(
            "'{}' matches multiple scopes ({}); use a more specific name.",
            wanted,
            matches
                .iter()
                .map(|&i| scopes[i].name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imscope::consumer::ScopeDomain;

    fn scope(name: &str) -> ScopeConfig {
        ScopeConfig {
            name: name.to_string(),
            group: String::new(),
            scope_type: imscope::consumer::ScopeType::Real,
            domain: ScopeDomain::Time,
        }
    }

    #[test]
    fn resolve_scope_idx_no_scopes_errs() {
        assert!(resolve_scope_idx(&[], None).is_err());
    }

    #[test]
    fn resolve_scope_idx_none_wanted_picks_first() {
        let scopes = vec![scope("a"), scope("b")];
        assert_eq!(resolve_scope_idx(&scopes, None), Ok(0));
    }

    #[test]
    fn resolve_scope_idx_exact_case_insensitive_match() {
        let scopes = vec![scope("Channel A"), scope("Channel B")];
        assert_eq!(resolve_scope_idx(&scopes, Some("channel b")), Ok(1));
    }

    #[test]
    fn resolve_scope_idx_unique_substring_match() {
        let scopes = vec![scope("ControlledWave"), scope("Other")];
        assert_eq!(resolve_scope_idx(&scopes, Some("wave")), Ok(0));
    }

    #[test]
    fn resolve_scope_idx_no_match_errs_with_available_names() {
        let scopes = vec![scope("a"), scope("b")];
        let err = resolve_scope_idx(&scopes, Some("nope")).unwrap_err();
        assert!(err.contains("a, b"));
    }

    #[test]
    fn resolve_scope_idx_ambiguous_substring_errs() {
        let scopes = vec![scope("Channel A"), scope("Channel B")];
        let err = resolve_scope_idx(&scopes, Some("channel")).unwrap_err();
        assert!(err.contains("Channel A"));
        assert!(err.contains("Channel B"));
    }
}

/// Connects just long enough to print the producer's scope list, for
/// scripting multiple `imscope-view` instances against the same producer.
fn run_list_mode(producer: &str, event_rx: &mpsc::Receiver<WorkerEvent>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("Timed out connecting to producer '{}'", producer));
        }
        match event_rx.recv_timeout(remaining) {
            Ok(WorkerEvent::Connected { name, scopes, .. }) => {
                println!("Producer: {}", name);
                for (i, s) in scopes.iter().enumerate() {
                    let group = if s.group.is_empty() {
                        "-"
                    } else {
                        s.group.as_str()
                    };
                    println!(
                        "  [{}] {}  type={:?}  domain={:?}  group={}",
                        i, s.name, s.scope_type, s.domain, group
                    );
                }
                return Ok(());
            }
            Ok(WorkerEvent::ConnectionFailed(err)) => return Err(err),
            Ok(_) => continue,
            Err(_) => return Err("Worker thread exited unexpectedly.".to_string()),
        }
    }
}

struct AppState {
    connection: ConnectionState,
    pane: PlotPane,
    plot_kind: PlotKind,
    /// Set when the requested --scope couldn't be resolved against the
    /// announced scope list; displayed instead of a plot.
    scope_error: Option<String>,
}

impl AppState {
    fn new(plot_kind: PlotKind) -> Self {
        Self {
            connection: ConnectionState::Disconnected(None),
            pane: PlotPane::new(),
            plot_kind,
            scope_error: None,
        }
    }
}

fn draw_ui(frame: &mut Frame, state: &AppState, producer: &str) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);

    let status = match &state.connection {
        ConnectionState::Disconnected(None) => format!("Connecting to producer '{}'...", producer),
        ConnectionState::Disconnected(Some(err)) => format!("Disconnected: {}", err),
        ConnectionState::Connecting => format!("Connecting to producer '{}'...", producer),
        ConnectionState::Connected { name, scopes, .. } => {
            let scope_name = scopes
                .get(state.pane.selected_scope_idx)
                .map(|s| s.name.as_str())
                .unwrap_or("?");
            format!(
                "Connected: '{}'  |  scope: {}  |  plot: {}",
                name,
                scope_name,
                state.plot_kind.title()
            )
        }
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::Cyan)),
        chunks[0],
    );

    let plot_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));

    if let Some(err) = &state.scope_error {
        let msg = Paragraph::new(format!("\n\n{}", err))
            .style(Style::default().fg(Color::Yellow))
            .block(plot_block);
        frame.render_widget(msg, chunks[1]);
        tui_render::render_metadata_footer(frame, chunks[2], None, None);
    } else if let ConnectionState::Connected { scopes, .. } = &state.connection {
        let scope_type = scopes
            .get(state.pane.selected_scope_idx)
            .map(|s| s.scope_type);
        let scope_domain = scopes.get(state.pane.selected_scope_idx).map(|s| s.domain);
        let snapshot = state.pane.active_snapshot.as_ref();
        if let Some(snapshot) = snapshot {
            tui_render::render_single_scope(
                frame,
                chunks[1],
                plot_block,
                state.plot_kind,
                snapshot,
                scope_type,
                scope_domain,
                "Waiting for data...",
            );
        } else {
            let msg = Paragraph::new("\n\nWaiting for first frame...")
                .style(Style::default().fg(Color::DarkGray))
                .block(plot_block);
            frame.render_widget(msg, chunks[1]);
        }
        tui_render::render_metadata_footer(frame, chunks[2], snapshot, scope_type);
    } else {
        let msg = Paragraph::new("\n\nNot connected yet.")
            .style(Style::default().fg(Color::DarkGray))
            .block(plot_block);
        frame.render_widget(msg, chunks[1]);
        tui_render::render_metadata_footer(frame, chunks[2], None, None);
    }

    frame.render_widget(
        Paragraph::new("q / Esc / Ctrl-C: quit    1-5: Scatter/RMS/Waveform/Histogram/Spectrum")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    thread::spawn(move || run_worker(cmd_rx, event_tx));

    let _ = cmd_tx.send(WorkerCommand::Connect {
        name: args.producer.clone(),
        locator: args.zenoh_connect.clone(),
    });

    if args.list {
        return run_list_mode(&args.producer, &event_rx).map_err(Into::into);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new(args.plot.into());
    let tick_rate = Duration::from_millis(50);
    let mut last_tick = Instant::now();

    let run_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        loop {
            while let Ok(event) = event_rx.try_recv() {
                match event {
                    WorkerEvent::Connecting => {
                        state.connection = ConnectionState::Connecting;
                    }
                    WorkerEvent::Connected {
                        name,
                        data_address,
                        control_address,
                        scopes,
                    } => {
                        match resolve_scope_idx(&scopes, args.scope.as_deref()) {
                            Ok(idx) => {
                                // Always show the single selected scope on its own,
                                // even if the producer put it in a group.
                                state.pane.ungrouped = true;
                                state.pane.stacking_enabled = args.stack;
                                state.pane.stacking_size = args.stacking_size;
                                activate_scope(&scopes, idx, &mut state.pane);
                                let _ = cmd_tx.send(WorkerCommand::SelectGroup {
                                    members: state.pane.worker_scopes.clone(),
                                });
                                state.scope_error = None;
                            }
                            Err(err) => state.scope_error = Some(err),
                        }
                        state.connection = ConnectionState::Connected {
                            name,
                            data_address,
                            control_address,
                            scopes,
                        };
                    }
                    WorkerEvent::ConnectionFailed(err) => {
                        state.connection = ConnectionState::Disconnected(Some(err));
                    }
                    WorkerEvent::NewData { scope_id, msg } => {
                        let stacking_enabled = state.pane.stacking_enabled;
                        if let Some(snapshot) = &mut state.pane.active_snapshot {
                            if snapshot.scope_id == scope_id as i32 {
                                snapshot.read_scope_msg(&msg, stacking_enabled);
                            }
                        }
                    }
                    WorkerEvent::Error(_)
                    | WorkerEvent::ScopesRefreshed { .. }
                    | WorkerEvent::SettingsRefreshed { .. }
                    | WorkerEvent::SettingUpdated { .. } => {}
                }
            }

            terminal.draw(|frame| draw_ui(frame, &state, &args.producer))?;

            let timeout = tick_rate.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break;
                            }
                            KeyCode::Char('1') => state.plot_kind = PlotKind::Scatter,
                            KeyCode::Char('2') => state.plot_kind = PlotKind::Rms,
                            KeyCode::Char('3') => state.plot_kind = PlotKind::Waveform,
                            KeyCode::Char('4') => state.plot_kind = PlotKind::Histogram,
                            KeyCode::Char('5') => state.plot_kind = PlotKind::Spectrum,
                            _ => {}
                        }
                    }
                }
            }
            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    run_result
}
