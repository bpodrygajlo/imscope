/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#![allow(dead_code)]

mod consumer;

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Bar, BarChart, BarGroup, Block, Borders, List, ListItem, ListState, Paragraph, Tabs,
        canvas::{Canvas, Points},
    },
};

use crate::consumer::{IQSnapshot, ScopeType, WorkerCommand, WorkerEvent, run_worker};

#[derive(Parser, Debug)]
#[command(author, version, about = "TUI client for imscope using Ratatui")]
struct Args {
    /// Address of the imscope announcer
    #[arg(short, long, default_value = "tcp://127.0.0.1:5557")]
    announce_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTab {
    Scatter = 0,
    Rms = 1,
    Waveform = 2,
    Histogram = 3,
}

impl AppTab {
    fn next(&self) -> Self {
        match self {
            AppTab::Scatter => AppTab::Rms,
            AppTab::Rms => AppTab::Waveform,
            AppTab::Waveform => AppTab::Histogram,
            AppTab::Histogram => AppTab::Scatter,
        }
    }

    fn prev(&self) -> Self {
        match self {
            AppTab::Scatter => AppTab::Histogram,
            AppTab::Rms => AppTab::Scatter,
            AppTab::Waveform => AppTab::Rms,
            AppTab::Histogram => AppTab::Waveform,
        }
    }

    fn as_index(&self) -> usize {
        *self as usize
    }
}

enum ConnectionState {
    Disconnected(Option<String>),
    Connecting,
    Connected {
        name: String,
        data_address: String,
        control_address: String,
        scopes: Vec<consumer::ScopeConfig>,
    },
}

struct AppState {
    connection: ConnectionState,
    announce_url: String,
    url_editing: bool,

    selected_scope_idx: usize,
    scope_list_state: ListState,

    active_tab: AppTab,

    // collection options
    auto_collect_enabled: bool,
    stacking_enabled: bool,
    stacking_size: usize,

    // filtering options
    filter_enabled: bool,
    filter_cutoff: f32,
    filter_percentage: f32,

    // active snapshot
    active_snapshot: Option<IQSnapshot>,

    // statistics
    frame_rate_counter: u32,
    last_frame_rate_calc: Instant,
    current_fps: f32,

    status_message: String,
    status_msg_time: Option<Instant>,
}

impl AppState {
    pub fn new(announce_url: String) -> Self {
        Self {
            connection: ConnectionState::Disconnected(None),
            announce_url,
            url_editing: false,
            selected_scope_idx: 0,
            scope_list_state: ListState::default(),
            active_tab: AppTab::Scatter,
            auto_collect_enabled: true,
            stacking_enabled: false,
            stacking_size: 16000,
            filter_enabled: false,
            filter_cutoff: 0.0,
            filter_percentage: 50.0,
            active_snapshot: None,
            frame_rate_counter: 0,
            last_frame_rate_calc: Instant::now(),
            current_fps: 0.0,
            status_message: "Press 'i' or 'e' to edit Connection URL, then 'Enter' to Connect"
                .to_string(),
            status_msg_time: Some(Instant::now()),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create communication channels
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();

    // Spawn worker thread
    thread::spawn(move || {
        run_worker(cmd_rx, event_tx);
    });

    // Initialize App State
    let mut app_state = AppState::new(args.announce_url.clone());

    // Trigger initial connection attempt
    app_state.status_message = format!("Connecting to {}...", args.announce_url);
    app_state.status_msg_time = Some(Instant::now());
    let _ = cmd_tx.send(WorkerCommand::Connect {
        url: args.announce_url,
    });

    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(33); // ~30 FPS

    loop {
        // 1. Process network events from worker
        while let Ok(event) = event_rx.try_recv() {
            match event {
                WorkerEvent::Connecting => {
                    app_state.connection = ConnectionState::Connecting;
                }
                WorkerEvent::Connected {
                    name,
                    data_address,
                    control_address,
                    scopes,
                } => {
                    app_state.connection = ConnectionState::Connected {
                        name,
                        data_address,
                        control_address,
                        scopes: scopes.clone(),
                    };
                    app_state.status_message = "Connected successfully!".to_string();
                    app_state.status_msg_time = Some(Instant::now());

                    // Select first scope by default
                    if !scopes.is_empty() {
                        app_state.selected_scope_idx = 0;
                        app_state.scope_list_state.select(Some(0));
                        let scope = &scopes[0];
                        let _ = cmd_tx.send(WorkerCommand::SelectScope {
                            scope_id: 0,
                            scope_type: scope.scope_type,
                        });

                        let mut snapshot = IQSnapshot::new(0);
                        snapshot.max_stacked_size = app_state.stacking_size;
                        app_state.active_snapshot = Some(snapshot);
                    }
                }
                WorkerEvent::ConnectionFailed(err) => {
                    app_state.connection = ConnectionState::Disconnected(Some(err.clone()));
                    app_state.status_message = format!("Connection failed: {}", err);
                    app_state.status_msg_time = Some(Instant::now());
                }
                WorkerEvent::NewData { scope_id, msg } => {
                    if let Some(ref mut snapshot) = app_state.active_snapshot {
                        if snapshot.scope_id == scope_id as i32 {
                            snapshot.read_scope_msg(msg, app_state.stacking_enabled);
                            app_state.frame_rate_counter += 1;
                        }
                    }
                }
                WorkerEvent::Error(err) => {
                    app_state.status_message = format!("Worker error: {}", err);
                    app_state.status_msg_time = Some(Instant::now());
                }
                WorkerEvent::ScopesRefreshed { scopes } => {
                    if let ConnectionState::Connected {
                        scopes: ref mut ref_mut_scopes,
                        ..
                    } = app_state.connection
                    {
                        *ref_mut_scopes = scopes.clone();
                        app_state.status_message = "Scopes refreshed successfully!".to_string();

                        // Keep selected_scope_idx within bounds
                        if !scopes.is_empty() {
                            if app_state.selected_scope_idx >= scopes.len() {
                                app_state.selected_scope_idx = 0;
                            }
                            app_state
                                .scope_list_state
                                .select(Some(app_state.selected_scope_idx));

                            let scope = &scopes[app_state.selected_scope_idx];
                            let _ = cmd_tx.send(WorkerCommand::SelectScope {
                                scope_id: app_state.selected_scope_idx,
                                scope_type: scope.scope_type,
                            });
                        } else {
                            app_state.scope_list_state.select(None);
                            app_state.active_snapshot = None;
                        }
                    } else {
                        app_state.status_message =
                            "Scopes refreshed, but not connected.".to_string();
                    }
                    app_state.status_msg_time = Some(Instant::now());
                }
            }
        }

        // Calculate FPS
        let elapsed = app_state.last_frame_rate_calc.elapsed();
        if elapsed >= Duration::from_secs(1) {
            app_state.current_fps = app_state.frame_rate_counter as f32 / elapsed.as_secs_f32();
            app_state.frame_rate_counter = 0;
            app_state.last_frame_rate_calc = Instant::now();
        }

        // 2. Poll for terminal/keyboard inputs
        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        if app_state.url_editing {
                            match key.code {
                                KeyCode::Enter => {
                                    app_state.url_editing = false;
                                    app_state.status_message =
                                        format!("Connecting to {}...", app_state.announce_url);
                                    app_state.status_msg_time = Some(Instant::now());
                                    let _ = cmd_tx.send(WorkerCommand::Connect {
                                        url: app_state.announce_url.clone(),
                                    });
                                }
                                KeyCode::Esc => {
                                    app_state.url_editing = false;
                                }
                                KeyCode::Backspace => {
                                    app_state.announce_url.pop();
                                }
                                KeyCode::Char(c) => {
                                    app_state.announce_url.push(c);
                                }
                                _ => {}
                            }
                        } else {
                            // General shortcut keys
                            match key.code {
                                KeyCode::Char('q') => break,
                                KeyCode::Char('c') => {
                                    app_state.status_message =
                                        format!("Reconnecting to {}...", app_state.announce_url);
                                    app_state.status_msg_time = Some(Instant::now());
                                    let _ = cmd_tx.send(WorkerCommand::Connect {
                                        url: app_state.announce_url.clone(),
                                    });
                                }
                                KeyCode::Char('e') | KeyCode::Char('i') => {
                                    app_state.url_editing = true;
                                }
                                KeyCode::Tab => {
                                    app_state.active_tab = app_state.active_tab.next();
                                }
                                KeyCode::BackTab => {
                                    app_state.active_tab = app_state.active_tab.prev();
                                }
                                KeyCode::Char('1') => app_state.active_tab = AppTab::Scatter,
                                KeyCode::Char('2') => app_state.active_tab = AppTab::Rms,
                                KeyCode::Char('3') => app_state.active_tab = AppTab::Waveform,
                                KeyCode::Char('4') => app_state.active_tab = AppTab::Histogram,

                                KeyCode::Up | KeyCode::Char('k') => {
                                    if let ConnectionState::Connected { ref scopes, .. } =
                                        app_state.connection
                                    {
                                        if !scopes.is_empty() {
                                            app_state.selected_scope_idx = app_state
                                                .selected_scope_idx
                                                .checked_sub(1)
                                                .unwrap_or(scopes.len() - 1);
                                            app_state
                                                .scope_list_state
                                                .select(Some(app_state.selected_scope_idx));

                                            let scope = &scopes[app_state.selected_scope_idx];
                                            let _ = cmd_tx.send(WorkerCommand::SelectScope {
                                                scope_id: app_state.selected_scope_idx,
                                                scope_type: scope.scope_type,
                                            });

                                            let mut snapshot = IQSnapshot::new(
                                                app_state.selected_scope_idx as i32,
                                            );
                                            snapshot.max_stacked_size = app_state.stacking_size;
                                            app_state.active_snapshot = Some(snapshot);
                                        }
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if let ConnectionState::Connected { ref scopes, .. } =
                                        app_state.connection
                                    {
                                        if !scopes.is_empty() {
                                            app_state.selected_scope_idx =
                                                (app_state.selected_scope_idx + 1) % scopes.len();
                                            app_state
                                                .scope_list_state
                                                .select(Some(app_state.selected_scope_idx));

                                            let scope = &scopes[app_state.selected_scope_idx];
                                            let _ = cmd_tx.send(WorkerCommand::SelectScope {
                                                scope_id: app_state.selected_scope_idx,
                                                scope_type: scope.scope_type,
                                            });

                                            let mut snapshot = IQSnapshot::new(
                                                app_state.selected_scope_idx as i32,
                                            );
                                            snapshot.max_stacked_size = app_state.stacking_size;
                                            app_state.active_snapshot = Some(snapshot);
                                        }
                                    }
                                }
                                KeyCode::Char('a') => {
                                    app_state.auto_collect_enabled =
                                        !app_state.auto_collect_enabled;
                                    let _ = cmd_tx.send(WorkerCommand::SetAutoCollect(
                                        app_state.auto_collect_enabled,
                                    ));
                                }
                                KeyCode::Char('s') => {
                                    app_state.stacking_enabled = !app_state.stacking_enabled;
                                }
                                KeyCode::Char('r') => {
                                    let _ = cmd_tx.send(WorkerCommand::RequestSingleFrame);
                                }
                                KeyCode::Char('R') => {
                                    app_state.status_message = "Refreshing scopes...".to_string();
                                    app_state.status_msg_time = Some(Instant::now());
                                    let _ = cmd_tx.send(WorkerCommand::RefreshScopes);
                                }
                                KeyCode::Char('f') => {
                                    app_state.filter_enabled = !app_state.filter_enabled;
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.filter_enabled,
                                        cutoff: app_state.filter_cutoff,
                                        percentage: app_state.filter_percentage,
                                    });
                                }
                                KeyCode::Char('+') | KeyCode::Char('=') => {
                                    app_state.stacking_size =
                                        (app_state.stacking_size + 1000).min(100000);
                                    if let Some(ref mut snapshot) = app_state.active_snapshot {
                                        snapshot.max_stacked_size = app_state.stacking_size;
                                    }
                                }
                                KeyCode::Char('-') | KeyCode::Char('_') => {
                                    app_state.stacking_size =
                                        (app_state.stacking_size.saturating_sub(1000)).max(1000);
                                    if let Some(ref mut snapshot) = app_state.active_snapshot {
                                        snapshot.max_stacked_size = app_state.stacking_size;
                                    }
                                }
                                KeyCode::Char(']') => {
                                    app_state.filter_cutoff =
                                        (app_state.filter_cutoff + 10.0).min(32767.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.filter_enabled,
                                        cutoff: app_state.filter_cutoff,
                                        percentage: app_state.filter_percentage,
                                    });
                                }
                                KeyCode::Char('[') => {
                                    app_state.filter_cutoff =
                                        (app_state.filter_cutoff - 10.0).max(0.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.filter_enabled,
                                        cutoff: app_state.filter_cutoff,
                                        percentage: app_state.filter_percentage,
                                    });
                                }
                                KeyCode::Char('}') => {
                                    app_state.filter_percentage =
                                        (app_state.filter_percentage + 5.0).min(100.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.filter_enabled,
                                        cutoff: app_state.filter_cutoff,
                                        percentage: app_state.filter_percentage,
                                    });
                                }
                                KeyCode::Char('{') => {
                                    app_state.filter_percentage =
                                        (app_state.filter_percentage - 5.0).max(0.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.filter_enabled,
                                        cutoff: app_state.filter_cutoff,
                                        percentage: app_state.filter_percentage,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    if mouse_event.kind == event::MouseEventKind::Down(event::MouseButton::Left) {
                        if let Ok(size) = terminal.size() {
                            handle_mouse_click(
                                mouse_event.column,
                                mouse_event.row,
                                Rect::new(0, 0, size.width, size.height),
                                &mut app_state,
                                &cmd_tx,
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        // 3. Render frame
        if last_tick.elapsed() >= tick_rate {
            terminal.draw(|f| draw_ui(f, &mut app_state))?;
            last_tick = Instant::now();
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn draw_ui(frame: &mut Frame, state: &mut AppState) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Content
            Constraint::Length(3), // Status bar
        ])
        .split(frame.area());

    // Render Header
    let fps_text = format!("FPS: {:.1}", state.current_fps);
    let title_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let title_paragraph = Paragraph::new(Line::from(vec![
        Span::raw(" 🛠️  ").fg(Color::Cyan),
        Span::styled(
            "IMScope TUI - Real-time IQ Signal Analyzer",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        ),
        Span::raw(" | ").fg(Color::DarkGray),
        Span::styled(fps_text, Style::default().fg(Color::Green)),
    ]))
    .block(title_block)
    .alignment(Alignment::Left);

    frame.render_widget(title_paragraph, main_chunks[0]);

    // Split Content into Left Sidebar and Right Plot Area
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(42), // Sidebar width
            Constraint::Min(20),    // Plot area
        ])
        .split(main_chunks[1]);

    draw_sidebar(frame, content_chunks[0], state);
    draw_plot_area(frame, content_chunks[1], state);

    // Render Status Bar
    let shortcut_text = "q: Quit | i: Edit URL | c: Connect | R: Refresh | a: Auto Collect | r: Request Single | f: Toggle Filter";
    let status_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            " [KEYS] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {} ", shortcut_text)),
        Span::raw(" | ").fg(Color::DarkGray),
        Span::styled(
            format!("Msg: {}", state.status_message),
            Style::default().fg(Color::Yellow),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(status_paragraph, main_chunks[2]);
}

fn draw_sidebar(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // Connection Block
            Constraint::Length(9), // Scopes List
            Constraint::Min(10),   // Control Settings
        ])
        .split(area);

    // 1. Connection Block
    let status_span = match &state.connection {
        ConnectionState::Disconnected(err_opt) => {
            if let Some(err) = err_opt {
                Span::styled(
                    format!("Disconnected ({})", err),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    "Disconnected",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )
            }
        }
        ConnectionState::Connecting => Span::styled(
            "Connecting...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        ConnectionState::Connected { name, .. } => Span::styled(
            format!("Connected ({})", name),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let url_style = if state.url_editing {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let conn_lines = vec![
        Line::from(vec![Span::raw("Status: "), status_span]),
        Line::from(vec![
            Span::raw("Announcer: "),
            Span::styled(&state.announce_url, url_style),
        ]),
        Line::from(vec![
            Span::raw(if state.url_editing {
                "✍️ Editing - press Enter to Connect"
            } else {
                "Press 'i' to edit connection URL"
            })
            .fg(Color::DarkGray),
        ]),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled(
                "R",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to refresh scopes").fg(Color::DarkGray),
        ]),
    ];

    let conn_block = Block::default()
        .borders(Borders::ALL)
        .title(" Connection Server ")
        .border_style(Style::default().fg(if state.url_editing {
            Color::Yellow
        } else {
            Color::DarkGray
        }));
    frame.render_widget(Paragraph::new(conn_lines).block(conn_block), chunks[0]);

    // 2. Scopes List Block
    let mut list_items = Vec::new();
    let mut active_scope_name = "None".to_string();
    let mut active_scope_type = ScopeType::IqData;

    if let ConnectionState::Connected { ref scopes, .. } = state.connection {
        for (i, scope) in scopes.iter().enumerate() {
            let type_str = match scope.scope_type {
                ScopeType::Real => "Real",
                ScopeType::IqData => "IQ",
            };

            let item_text = format!("{:02}. {} [{}]", i, scope.name, type_str);
            let style = if i == state.selected_scope_idx {
                active_scope_name = scope.name.clone();
                active_scope_type = scope.scope_type;
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            list_items.push(ListItem::new(item_text).style(style));
        }
    } else {
        list_items.push(ListItem::new("No connected scopes").fg(Color::DarkGray));
    }

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(" Available Scopes (▲/▼) ")
        .border_style(Style::default().fg(Color::DarkGray));

    let list = List::new(list_items).block(list_block);
    frame.render_stateful_widget(list, chunks[1], &mut state.scope_list_state);

    // 3. Settings Block
    let auto_status = if state.auto_collect_enabled {
        Span::styled(
            " [x] ON  (Running)",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [ ] OFF (Idle)", Style::default().fg(Color::Red))
    };

    let stacking_status = if state.stacking_enabled {
        Span::styled(
            " [x] ENABLED",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [ ] DISABLED", Style::default().fg(Color::DarkGray))
    };

    let filter_status = if state.filter_enabled {
        Span::styled(
            " [x] ENABLED",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [ ] DISABLED", Style::default().fg(Color::DarkGray))
    };

    let mut settings_lines = vec![
        Line::from(vec![
            Span::styled(
                "Auto Collect ('a'):",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            auto_status,
        ]),
        Line::from(vec![
            Span::styled(
                "Stacking ('s'):    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            stacking_status,
        ]),
        Line::from(vec![
            Span::raw("Stacking size: "),
            Span::styled(
                format!("{:>5}", state.stacking_size),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(" [-] ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [+] ", Style::default().fg(Color::White).bg(Color::Green)),
        ]),
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::styled(
                "Noise Filter ('f'):",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            filter_status,
        ]),
        Line::from(vec![
            Span::raw("Cutoff linear: "),
            Span::styled(
                format!("{:>5.0}", state.filter_cutoff),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(" [-] ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [+] ", Style::default().fg(Color::White).bg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("Max noise %:   "),
            Span::styled(
                format!("{:>4.0}%", state.filter_percentage),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(" [-] ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [+] ", Style::default().fg(Color::White).bg(Color::Green)),
        ]),
        Line::from(Span::raw("")),
        Line::from(vec![Span::styled(
            "Active Scope Info:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        Line::from(vec![
            Span::raw("Name: "),
            Span::styled(active_scope_name, Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("Type: "),
            Span::styled(
                format!("{:?}", active_scope_type),
                Style::default().fg(Color::Green),
            ),
        ]),
    ];

    if let Some(ref snapshot) = state.active_snapshot {
        settings_lines.push(Line::from(vec![
            Span::raw("BufferSize: "),
            Span::styled(
                format!("{}", snapshot.size()),
                Style::default().fg(Color::Green),
            ),
        ]));
    }

    let settings_block = Block::default()
        .borders(Borders::ALL)
        .title(" Controls & Settings ")
        .border_style(Style::default().fg(Color::DarkGray));

    frame.render_widget(
        Paragraph::new(settings_lines).block(settings_block),
        chunks[2],
    );
}

fn draw_plot_area(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Tab bar
            Constraint::Min(10),   // Plot canvas
            Constraint::Length(5), // Metadata block
        ])
        .split(area);

    // 1. Render Tabs
    let tab_titles = vec![
        "1. Scatter (IQ)",
        "2. RMS Power (IQ)",
        "3. Waveform",
        "4. Histogram",
    ];
    let tab_style = Style::default().fg(Color::White);
    let selected_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Plot Select (Tab) "),
        )
        .select(state.active_tab.as_index())
        .style(tab_style)
        .highlight_style(selected_style);

    frame.render_widget(tabs, chunks[0]);

    // 2. Render selected Plot
    let plot_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    if let Some(ref snapshot) = state.active_snapshot {
        if snapshot.size() == 0 {
            let no_data_msg = Paragraph::new("\n\nNo scope data received yet.\nEnable 'Auto Collect' or press 'r' to trigger request.")
                .alignment(Alignment::Center)
                .fg(Color::DarkGray)
                .block(plot_block);
            frame.render_widget(no_data_msg, chunks[1]);
        } else {
            match state.active_tab {
                AppTab::Scatter => {
                    if let ConnectionState::Connected { ref scopes, .. } = state.connection {
                        if scopes[state.selected_scope_idx].scope_type == ScopeType::Real {
                            let msg = Paragraph::new(
                                "\n\nScatter/Constellation plot is only available for IQ scopes.",
                            )
                            .alignment(Alignment::Center)
                            .fg(Color::Yellow)
                            .block(plot_block);
                            frame.render_widget(msg, chunks[1]);
                            return;
                        }
                    }

                    let limit = (snapshot.max_iq as f64 * 1.2).max(1.0);
                    let skip = (snapshot.real.len() / 2000).max(1);
                    let points = snapshot
                        .real
                        .iter()
                        .zip(snapshot.imag.iter())
                        .step_by(skip)
                        .map(|(&r, &im)| (r as f64, im as f64))
                        .collect::<Vec<(f64, f64)>>();

                    let canvas = Canvas::default()
                        .block(plot_block.title(" Constellation Scatterplot (Imag vs Real) "))
                        .x_bounds([-limit, limit])
                        .y_bounds([-limit, limit])
                        .paint(move |ctx| {
                            // Draw Grid/Axes
                            ctx.draw(&ratatui::widgets::canvas::Line {
                                x1: -limit,
                                y1: 0.0,
                                x2: limit,
                                y2: 0.0,
                                color: Color::DarkGray,
                            });
                            ctx.draw(&ratatui::widgets::canvas::Line {
                                x1: 0.0,
                                y1: -limit,
                                x2: 0.0,
                                y2: limit,
                                color: Color::DarkGray,
                            });
                            // Draw Points
                            ctx.draw(&Points {
                                coords: &points,
                                color: Color::Cyan,
                            });

                            // Labels
                            ctx.print(
                                -limit * 0.95,
                                limit * 0.85,
                                format!("Max IQ: {:.0}", limit).fg(Color::Gray),
                            );
                        });
                    frame.render_widget(canvas, chunks[1]);
                }
                AppTab::Rms => {
                    if let ConnectionState::Connected { ref scopes, .. } = state.connection {
                        if scopes[state.selected_scope_idx].scope_type == ScopeType::Real {
                            let msg = Paragraph::new(
                                "\n\nRMS Power plot is only available for IQ scopes.",
                            )
                            .alignment(Alignment::Center)
                            .fg(Color::Yellow)
                            .block(plot_block);
                            frame.render_widget(msg, chunks[1]);
                            return;
                        }
                    }

                    let max_p = (snapshot.max_power as f64 * 1.1).max(1.0);
                    let num_samples = snapshot.power.len();

                    let canvas = Canvas::default()
                        .block(plot_block.title(" RMS Power over Samples (r^2 + im^2) "))
                        .x_bounds([0.0, num_samples as f64])
                        .y_bounds([0.0, max_p])
                        .paint(move |ctx| {
                            // Draw grid lines
                            ctx.draw(&ratatui::widgets::canvas::Line {
                                x1: 0.0,
                                y1: max_p * 0.5,
                                x2: num_samples as f64,
                                y2: max_p * 0.5,
                                color: Color::Indexed(236),
                            });

                            // Plot power line
                            let step = (num_samples / 500).max(1);
                            for i in (0..num_samples.saturating_sub(step)).step_by(step) {
                                ctx.draw(&ratatui::widgets::canvas::Line {
                                    x1: i as f64,
                                    y1: snapshot.power[i] as f64,
                                    x2: (i + step) as f64,
                                    y2: snapshot.power[i + step] as f64,
                                    color: Color::Yellow,
                                });
                            }

                            // Labels
                            ctx.print(
                                0.0,
                                max_p * 0.9,
                                format!("Max Power: {:.0}", max_p).fg(Color::Gray),
                            );
                        });
                    frame.render_widget(canvas, chunks[1]);
                }
                AppTab::Waveform => {
                    let limit = (snapshot.max_iq as f64 * 1.1).max(1.0);
                    let num_samples = snapshot.real.len();

                    let canvas = Canvas::default()
                        .block(plot_block.title(" Real Amplitudes Waveform "))
                        .x_bounds([0.0, num_samples as f64])
                        .y_bounds([-limit, limit])
                        .paint(move |ctx| {
                            // Center line
                            ctx.draw(&ratatui::widgets::canvas::Line {
                                x1: 0.0,
                                y1: 0.0,
                                x2: num_samples as f64,
                                y2: 0.0,
                                color: Color::DarkGray,
                            });

                            // Plot waveform
                            let step = (num_samples / 500).max(1);
                            for i in (0..num_samples.saturating_sub(step)).step_by(step) {
                                ctx.draw(&ratatui::widgets::canvas::Line {
                                    x1: i as f64,
                                    y1: snapshot.real[i] as f64,
                                    x2: (i + step) as f64,
                                    y2: snapshot.real[i + step] as f64,
                                    color: Color::Green,
                                });
                            }

                            // Labels
                            ctx.print(
                                0.0,
                                limit * 0.8,
                                format!("Limit: ±{:.0}", limit).fg(Color::Gray),
                            );
                        });
                    frame.render_widget(canvas, chunks[1]);
                }
                AppTab::Histogram => {
                    let num_bins = 20;
                    let mut bin_counts = vec![0u64; num_bins];
                    let limit = snapshot.max_iq.max(1) as f32;
                    let bin_width = (2.0 * limit) / (num_bins as f32);

                    for &val in &snapshot.real {
                        let float_val = val as f32;
                        let idx = (((float_val + limit) / bin_width) as usize).min(num_bins - 1);
                        bin_counts[idx] += 1;
                    }

                    let bars: Vec<Bar> = (0..num_bins)
                        .map(|i| {
                            let bin_center = -limit + (i as f32 + 0.5) * bin_width;
                            let label = format!("{:.0}", bin_center);
                            Bar::default()
                                .label(label)
                                .value(bin_counts[i])
                                .style(Style::default().fg(Color::Magenta))
                        })
                        .collect();

                    let chart_group = BarGroup::default().bars(&bars);

                    let chart = BarChart::default()
                        .block(
                            plot_block
                                .title(" Amplitude Density Distribution (1D Real Histogram) "),
                        )
                        .data(chart_group)
                        .bar_width(3)
                        .bar_gap(1)
                        .value_style(
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )
                        .label_style(Style::default().fg(Color::Gray));

                    frame.render_widget(chart, chunks[1]);
                }
            }
        }
    } else {
        let no_conn_msg = Paragraph::new("\n\nNot connected to any producer.\nPlease enter announcer address on the left and connect.")
            .alignment(Alignment::Center)
            .fg(Color::DarkGray)
            .block(plot_block);
        frame.render_widget(no_conn_msg, chunks[1]);
    }

    // 3. Render Metadata footer
    let meta_block = Block::default()
        .borders(Borders::ALL)
        .title(" Active Frame Metadata ")
        .border_style(Style::default().fg(Color::DarkGray));

    if let Some(ref snapshot) = state.active_snapshot {
        let meta_lines = vec![
            Line::from(vec![
                Span::raw("Slot: "),
                Span::styled(
                    format!("{}", snapshot.meta.slot),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("   Frame: "),
                Span::styled(
                    format!("{}", snapshot.meta.frame),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("   Timestamp: "),
                Span::styled(
                    format!("{}", snapshot.meta.timestamp),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("Max absolute IQ value: "),
                Span::styled(
                    format!("{}", snapshot.max_iq),
                    Style::default().fg(Color::Green),
                ),
                Span::raw("   Non-zero samples count: "),
                Span::styled(
                    format!("{}", snapshot.nonzero_count),
                    Style::default().fg(Color::Green),
                ),
                Span::raw("   Total stacked samples: "),
                Span::styled(
                    format!("{}", snapshot.size()),
                    Style::default().fg(Color::Green),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(meta_lines).block(meta_block), chunks[2]);
    } else {
        frame.render_widget(
            Paragraph::new("No active scope metadata.")
                .fg(Color::DarkGray)
                .block(meta_block),
            chunks[2],
        );
    }
}

fn handle_mouse_click(
    col: u16,
    row: u16,
    area: Rect,
    state: &mut AppState,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Content
            Constraint::Length(3), // Status bar
        ])
        .split(area);

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(42), // Sidebar width
            Constraint::Min(20),    // Plot area
        ])
        .split(main_chunks[1]);

    let sidebar_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // Connection Block
            Constraint::Length(9), // Scopes List
            Constraint::Min(10),   // Control Settings
        ])
        .split(content_chunks[0]);

    let plot_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Tab bar
            Constraint::Min(10),   // Plot canvas
            Constraint::Length(5), // Metadata block
        ])
        .split(content_chunks[1]);

    // 1. Check if clicked inside Connection Block
    let conn = sidebar_chunks[0];
    if col >= conn.x && col < conn.x + conn.width && row >= conn.y && row < conn.y + conn.height {
        if row == conn.y + 2 {
            state.url_editing = true;
        } else if row == conn.y + 1 {
            state.status_message = format!("Reconnecting to {}...", state.announce_url);
            state.status_msg_time = Some(Instant::now());
            let _ = cmd_tx.send(WorkerCommand::Connect {
                url: state.announce_url.clone(),
            });
        } else if row == conn.y + 4 {
            state.status_message = "Refreshing scopes...".to_string();
            state.status_msg_time = Some(Instant::now());
            let _ = cmd_tx.send(WorkerCommand::RefreshScopes);
        }
        return;
    }

    // 2. Check if clicked inside Scopes List
    let scopes_rect = sidebar_chunks[1];
    if col >= scopes_rect.x
        && col < scopes_rect.x + scopes_rect.width
        && row >= scopes_rect.y
        && row < scopes_rect.y + scopes_rect.height
    {
        if let ConnectionState::Connected { ref scopes, .. } = state.connection {
            let list_start_y = scopes_rect.y + 1;
            let list_end_y = scopes_rect.y + scopes_rect.height - 1;
            if row >= list_start_y && row < list_end_y {
                let clicked_idx = (row - list_start_y) as usize;
                if clicked_idx < scopes.len() {
                    state.selected_scope_idx = clicked_idx;
                    state.scope_list_state.select(Some(clicked_idx));

                    let scope = &scopes[clicked_idx];
                    let _ = cmd_tx.send(WorkerCommand::SelectScope {
                        scope_id: clicked_idx,
                        scope_type: scope.scope_type,
                    });

                    let mut snapshot = IQSnapshot::new(clicked_idx as i32);
                    snapshot.max_stacked_size = state.stacking_size;
                    state.active_snapshot = Some(snapshot);
                }
            }
        }
        return;
    }

    // 3. Check if clicked inside Controls Settings
    let controls = sidebar_chunks[2];
    if col >= controls.x
        && col < controls.x + controls.width
        && row >= controls.y
        && row < controls.y + controls.height
    {
        let relative_row = row as i32 - controls.y as i32 - 1;
        match relative_row {
            0 => {
                state.auto_collect_enabled = !state.auto_collect_enabled;
                let _ = cmd_tx.send(WorkerCommand::SetAutoCollect(state.auto_collect_enabled));
            }
            1 => {
                state.stacking_enabled = !state.stacking_enabled;
            }
            2 => {
                let relative_col = col as i32 - controls.x as i32 - 1;
                if relative_col >= 22 && relative_col <= 26 {
                    state.stacking_size = (state.stacking_size.saturating_sub(1000)).max(1000);
                    if let Some(ref mut snapshot) = state.active_snapshot {
                        snapshot.max_stacked_size = state.stacking_size;
                    }
                } else if relative_col >= 28 && relative_col <= 32 {
                    state.stacking_size = (state.stacking_size + 1000).min(100000);
                    if let Some(ref mut snapshot) = state.active_snapshot {
                        snapshot.max_stacked_size = state.stacking_size;
                    }
                }
            }
            4 => {
                state.filter_enabled = !state.filter_enabled;
                let _ = cmd_tx.send(WorkerCommand::SetFilter {
                    enabled: state.filter_enabled,
                    cutoff: state.filter_cutoff,
                    percentage: state.filter_percentage,
                });
            }
            5 => {
                let relative_col = col as i32 - controls.x as i32 - 1;
                if relative_col >= 22 && relative_col <= 26 {
                    state.filter_cutoff = (state.filter_cutoff - 10.0).max(0.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.filter_enabled,
                        cutoff: state.filter_cutoff,
                        percentage: state.filter_percentage,
                    });
                } else if relative_col >= 28 && relative_col <= 32 {
                    state.filter_cutoff = (state.filter_cutoff + 10.0).min(32767.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.filter_enabled,
                        cutoff: state.filter_cutoff,
                        percentage: state.filter_percentage,
                    });
                }
            }
            6 => {
                let relative_col = col as i32 - controls.x as i32 - 1;
                if relative_col >= 22 && relative_col <= 26 {
                    state.filter_percentage = (state.filter_percentage - 5.0).max(0.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.filter_enabled,
                        cutoff: state.filter_cutoff,
                        percentage: state.filter_percentage,
                    });
                } else if relative_col >= 28 && relative_col <= 32 {
                    state.filter_percentage = (state.filter_percentage + 5.0).min(100.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.filter_enabled,
                        cutoff: state.filter_cutoff,
                        percentage: state.filter_percentage,
                    });
                }
            }
            _ => {}
        }
        return;
    }

    // 4. Check if clicked inside Tab bar
    let tab_bar = plot_chunks[0];
    if col >= tab_bar.x
        && col < tab_bar.x + tab_bar.width
        && row >= tab_bar.y
        && row < tab_bar.y + tab_bar.height
    {
        let relative_col = col as i32 - tab_bar.x as i32 - 1;
        if relative_col >= 0 {
            state.active_tab = if relative_col < 17 {
                AppTab::Scatter
            } else if relative_col < 37 {
                AppTab::Rms
            } else if relative_col < 51 {
                AppTab::Waveform
            } else {
                AppTab::Histogram
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_mouse_click_tabs() {
        let mut state = AppState::new("tcp://127.0.0.1:5557".to_string());
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 120, 24);

        // Plot tab bar is located in plot_chunks[0]
        // main_chunks: height constraints [3, Min(10), 3]
        // content_chunks: horizontal constraints [42, Min(20)]
        // Plot area is content_chunks[1] (x=42, width=78)
        // plot_chunks: vertical constraints [3, Min(10), 5] inside content_chunks[1]
        // So tab_bar has x=42, y=3, width=78, height=3.

        // Clicking tab 2 (Waveform)
        // tab_bar is at x=42, y=3.
        // Waveform starts at relative_col = 38.
        // col = 42 + 1 + 38 = 81.
        // row = 3 + 1 = 4.
        handle_mouse_click(81, 4, area, &mut state, &tx);

        assert_eq!(state.active_tab, AppTab::Waveform);
    }

    #[test]
    fn test_handle_mouse_click_refresh() {
        let mut state = AppState::new("tcp://127.0.0.1:5557".to_string());
        let (tx, rx) = mpsc::channel();
        let area = Rect::new(0, 0, 120, 35);

        handle_mouse_click(5, 7, area, &mut state, &tx);

        // Check that command was sent
        let cmd = rx.try_recv().unwrap();
        match cmd {
            WorkerCommand::RefreshScopes => {}
            _ => panic!("Expected RefreshScopes command"),
        }
        assert_eq!(state.status_message, "Refreshing scopes...");
    }
}
