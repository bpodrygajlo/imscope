/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
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

use imscope::consumer::{
    self, IQSnapshot, ScopeConfig, ScopeType, WorkerCommand, WorkerEvent, run_worker,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "TUI client for imscope using Ratatui")]
struct Args {
    #[arg(short, long, default_value = "tcp://127.0.0.1:5557")]
    announce_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTab {
    Scatter = 0,
    Rms = 1,
    Waveform = 2,
    Histogram = 3,
    Settings = 4,
}

impl AppTab {
    fn next(&self) -> Self {
        match self {
            AppTab::Scatter => AppTab::Rms,
            AppTab::Rms => AppTab::Waveform,
            AppTab::Waveform => AppTab::Histogram,
            AppTab::Histogram => AppTab::Settings,
            AppTab::Settings => AppTab::Scatter,
        }
    }

    fn prev(&self) -> Self {
        match self {
            AppTab::Scatter => AppTab::Settings,
            AppTab::Rms => AppTab::Scatter,
            AppTab::Waveform => AppTab::Rms,
            AppTab::Histogram => AppTab::Waveform,
            AppTab::Settings => AppTab::Histogram,
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

// ── Per-plot-pane state ────────────────────────────────────────────────────────

struct PlotPane {
    selected_scope_idx: usize,
    scope_list_state: ListState,
    active_tab: AppTab,
    last_active_plot_tab: AppTab,
    stacking_enabled: bool,
    stacking_size: usize,
    filter_enabled: bool,
    filter_cutoff: f32,
    filter_percentage: f32,
    active_snapshot: Option<IQSnapshot>,
    group_snapshots: HashMap<usize, IQSnapshot>,
    in_group_mode: bool,
    ungrouped: bool,
    /// Scope IDs (and their types) this pane contributes to the worker's fetch list.
    worker_scopes: Vec<(usize, ScopeType)>,
}

impl PlotPane {
    fn new() -> Self {
        Self {
            selected_scope_idx: 0,
            scope_list_state: ListState::default(),
            active_tab: AppTab::Waveform,
            last_active_plot_tab: AppTab::Waveform,
            stacking_enabled: false,
            stacking_size: 16000,
            filter_enabled: false,
            filter_cutoff: 0.0,
            filter_percentage: 50.0,
            active_snapshot: None,
            group_snapshots: HashMap::new(),
            in_group_mode: false,
            ungrouped: false,
            worker_scopes: Vec::new(),
        }
    }
}

// ── Application state ──────────────────────────────────────────────────────────

struct AppState {
    connection: ConnectionState,
    announce_url: String,
    url_editing: bool,

    panes: [PlotPane; 2],
    num_panes: usize,   // 0, 1, or 2
    active_pane: usize, // index of the pane that receives keyboard input

    auto_collect_enabled: bool,

    frame_rate_counter: u32,
    last_frame_rate_calc: Instant,
    current_fps: f32,

    status_message: String,
    status_msg_time: Option<Instant>,

    settings: Vec<consumer::SettingInfo>,
    pending_settings: HashSet<String>,
    editing_setting_idx: Option<usize>,
    editing_setting_value: String,
    selected_setting_idx: usize,
}

impl AppState {
    pub fn new(announce_url: String) -> Self {
        Self {
            connection: ConnectionState::Disconnected(None),
            announce_url,
            url_editing: false,
            panes: [PlotPane::new(), PlotPane::new()],
            num_panes: 1,
            active_pane: 0,
            auto_collect_enabled: true,
            frame_rate_counter: 0,
            last_frame_rate_calc: Instant::now(),
            current_fps: 0.0,
            status_message: "Press 'i' or 'e' to edit Connection URL, then 'Enter' to Connect"
                .to_string(),
            status_msg_time: Some(Instant::now()),
            settings: Vec::new(),
            pending_settings: HashSet::new(),
            editing_setting_idx: None,
            editing_setting_value: String::new(),
            selected_setting_idx: 0,
        }
    }
}

const GROUP_COLORS: [Color; 8] = [
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Red,
    Color::Blue,
    Color::LightCyan,
    Color::LightYellow,
];

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Activate scope `idx` for `pane`: updates worker_scopes, snapshots, and group state.
fn activate_scope(scopes: &[ScopeConfig], idx: usize, pane: &mut PlotPane) {
    let group = &scopes[idx].group;
    if !group.is_empty() && !pane.ungrouped {
        let members: Vec<(usize, ScopeType)> = scopes
            .iter()
            .enumerate()
            .filter(|(_, s)| &s.group == group)
            .map(|(i, s)| (i, s.scope_type))
            .collect();
        pane.group_snapshots.clear();
        for &(id, _) in &members {
            let mut snap = IQSnapshot::new(id as i32);
            snap.max_stacked_size = pane.stacking_size;
            pane.group_snapshots.insert(id, snap);
        }
        pane.worker_scopes = members;
        pane.active_snapshot = None;
        pane.in_group_mode = true;
    } else {
        let mut snap = IQSnapshot::new(idx as i32);
        snap.max_stacked_size = pane.stacking_size;
        pane.worker_scopes = vec![(idx, scopes[idx].scope_type)];
        pane.active_snapshot = Some(snap);
        pane.group_snapshots.clear();
        pane.in_group_mode = false;
    }
    pane.selected_scope_idx = idx;
    pane.scope_list_state.select(Some(idx));
}

/// Merge all active panes' worker_scopes (deduped) and tell the worker to collect them.
fn send_merged_scopes(
    panes: &[PlotPane; 2],
    num_panes: usize,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
) {
    let mut all: Vec<(usize, ScopeType)> = Vec::new();
    let mut seen = HashSet::new();
    for pane in &panes[..num_panes] {
        for &(id, stype) in &pane.worker_scopes {
            if seen.insert(id) {
                all.push((id, stype));
            }
        }
    }
    let _ = cmd_tx.send(WorkerCommand::SelectGroup { members: all });
}

// ── Main ───────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();

    thread::spawn(move || {
        run_worker(cmd_rx, event_tx);
    });

    let mut app_state = AppState::new(args.announce_url.clone());

    app_state.status_message = format!("Connecting to {}...", args.announce_url);
    app_state.status_msg_time = Some(Instant::now());
    let _ = cmd_tx.send(WorkerCommand::Connect {
        url: args.announce_url,
    });

    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(33);

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

                    if !scopes.is_empty() {
                        for pane_idx in 0..app_state.num_panes {
                            let scope_idx = if pane_idx == 0 {
                                0
                            } else {
                                (app_state.panes[0].selected_scope_idx + 1) % scopes.len()
                            };
                            activate_scope(&scopes, scope_idx, &mut app_state.panes[pane_idx]);
                        }
                        send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx);
                    }
                }
                WorkerEvent::ConnectionFailed(err) => {
                    app_state.connection = ConnectionState::Disconnected(Some(err.clone()));
                    app_state.status_message = format!("Connection failed: {}", err);
                    app_state.status_msg_time = Some(Instant::now());
                }
                WorkerEvent::NewData { scope_id, msg } => {
                    let mut routed = false;
                    'route: for pane in &mut app_state.panes[..app_state.num_panes] {
                        for &(id, _) in &pane.worker_scopes {
                            if id == scope_id {
                                if pane.in_group_mode {
                                    if let Some(snap) = pane.group_snapshots.get_mut(&scope_id) {
                                        snap.read_scope_msg(msg, pane.stacking_enabled);
                                        routed = true;
                                    }
                                } else if let Some(ref mut snapshot) = pane.active_snapshot {
                                    if snapshot.scope_id == scope_id as i32 {
                                        snapshot.read_scope_msg(msg, pane.stacking_enabled);
                                        routed = true;
                                    }
                                }
                                break 'route;
                            }
                        }
                    }
                    if routed {
                        app_state.frame_rate_counter += 1;
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

                        if !scopes.is_empty() {
                            for pane_idx in 0..app_state.num_panes {
                                let idx = app_state.panes[pane_idx]
                                    .selected_scope_idx
                                    .min(scopes.len() - 1);
                                activate_scope(&scopes, idx, &mut app_state.panes[pane_idx]);
                            }
                            send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx);
                        } else {
                            for pane in &mut app_state.panes {
                                pane.scope_list_state.select(None);
                                pane.active_snapshot = None;
                                pane.group_snapshots.clear();
                                pane.in_group_mode = false;
                                pane.worker_scopes.clear();
                            }
                            send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx);
                        }
                    } else {
                        app_state.status_message =
                            "Scopes refreshed, but not connected.".to_string();
                    }
                    app_state.status_msg_time = Some(Instant::now());
                }
                WorkerEvent::SettingsRefreshed { settings } => {
                    app_state.settings = settings;
                }
                WorkerEvent::SettingUpdated { name, status } => {
                    app_state.pending_settings.remove(&name);
                    if status == 0 {
                        app_state.status_message = format!("Setting '{}' updated!", name);
                    } else {
                        app_state.status_message =
                            format!("Setting '{}' update failed ({})", name, status);
                    }
                    app_state.status_msg_time = Some(Instant::now());
                    let _ = cmd_tx.send(WorkerCommand::GetSettings);
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
                        } else if let Some(idx) = app_state.editing_setting_idx {
                            match key.code {
                                KeyCode::Enter => {
                                    if idx < app_state.settings.len() {
                                        let name = app_state.settings[idx].name.clone();
                                        let stype = app_state.settings[idx].setting_type;
                                        let parsed_val = match stype {
                                            consumer::SettingType::Int32 => {
                                                if let Ok(val) = app_state
                                                    .editing_setting_value
                                                    .trim()
                                                    .parse::<i32>()
                                                {
                                                    Some(consumer::SettingValue::Int32(val))
                                                } else {
                                                    app_state.status_message =
                                                        "Invalid Int32 value!".to_string();
                                                    app_state.status_msg_time =
                                                        Some(Instant::now());
                                                    None
                                                }
                                            }
                                            consumer::SettingType::Float => {
                                                if let Ok(val) = app_state
                                                    .editing_setting_value
                                                    .trim()
                                                    .parse::<f32>()
                                                {
                                                    Some(consumer::SettingValue::Float(val))
                                                } else {
                                                    app_state.status_message =
                                                        "Invalid Float value!".to_string();
                                                    app_state.status_msg_time =
                                                        Some(Instant::now());
                                                    None
                                                }
                                            }
                                            _ => None,
                                        };
                                        if let Some(val) = parsed_val {
                                            app_state.pending_settings.insert(name.clone());
                                            let _ = cmd_tx.send(
                                                consumer::WorkerCommand::UpdateSetting {
                                                    name,
                                                    setting_type: stype,
                                                    value: val,
                                                },
                                            );
                                            app_state.editing_setting_idx = None;
                                        }
                                    }
                                }
                                KeyCode::Esc => {
                                    app_state.editing_setting_idx = None;
                                }
                                KeyCode::Backspace => {
                                    app_state.editing_setting_value.pop();
                                }
                                KeyCode::Char(c) => {
                                    app_state.editing_setting_value.push(c);
                                }
                                _ => {}
                            }
                        } else {
                            let ap = app_state.active_pane;
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
                                // ── Pane management ──────────────────────────
                                KeyCode::Char('p') => {
                                    if app_state.num_panes == 2 {
                                        app_state.active_pane = 1 - app_state.active_pane;
                                    }
                                }
                                KeyCode::Char('P') => {
                                    // Cycle: 1 → 2 → 0 → 1
                                    app_state.num_panes = (app_state.num_panes + 1) % 3;
                                    match app_state.num_panes {
                                        0 => {
                                            send_merged_scopes(&app_state.panes, 0, &cmd_tx);
                                            app_state.active_pane = 0;
                                        }
                                        1 => {
                                            send_merged_scopes(&app_state.panes, 1, &cmd_tx);
                                            app_state.active_pane = 0;
                                        }
                                        2 => {
                                            if let ConnectionState::Connected {
                                                ref scopes, ..
                                            } = app_state.connection
                                            {
                                                if !scopes.is_empty()
                                                    && app_state.panes[1].worker_scopes.is_empty()
                                                {
                                                    let next =
                                                        (app_state.panes[0].selected_scope_idx + 1)
                                                            % scopes.len();
                                                    activate_scope(
                                                        scopes,
                                                        next,
                                                        &mut app_state.panes[1],
                                                    );
                                                }
                                            }
                                            app_state.active_pane = 1;
                                            send_merged_scopes(
                                                &app_state.panes,
                                                app_state.num_panes,
                                                &cmd_tx,
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                                // ── Tab navigation (active pane) ─────────────
                                KeyCode::Tab => {
                                    let old_tab = app_state.panes[ap].active_tab;
                                    if old_tab != AppTab::Settings {
                                        app_state.panes[ap].last_active_plot_tab = old_tab;
                                    }
                                    app_state.panes[ap].active_tab =
                                        app_state.panes[ap].active_tab.next();
                                    if app_state.panes[ap].active_tab == AppTab::Settings {
                                        let _ = cmd_tx.send(consumer::WorkerCommand::GetSettings);
                                    }
                                }
                                KeyCode::BackTab => {
                                    let old_tab = app_state.panes[ap].active_tab;
                                    if old_tab != AppTab::Settings {
                                        app_state.panes[ap].last_active_plot_tab = old_tab;
                                    }
                                    app_state.panes[ap].active_tab =
                                        app_state.panes[ap].active_tab.prev();
                                    if app_state.panes[ap].active_tab == AppTab::Settings {
                                        let _ = cmd_tx.send(consumer::WorkerCommand::GetSettings);
                                    }
                                }
                                KeyCode::Char('1') => {
                                    app_state.panes[ap].active_tab = AppTab::Scatter;
                                    app_state.panes[ap].last_active_plot_tab = AppTab::Scatter;
                                }
                                KeyCode::Char('2') => {
                                    app_state.panes[ap].active_tab = AppTab::Rms;
                                    app_state.panes[ap].last_active_plot_tab = AppTab::Rms;
                                }
                                KeyCode::Char('3') => {
                                    app_state.panes[ap].active_tab = AppTab::Waveform;
                                    app_state.panes[ap].last_active_plot_tab = AppTab::Waveform;
                                }
                                KeyCode::Char('4') => {
                                    app_state.panes[ap].active_tab = AppTab::Histogram;
                                    app_state.panes[ap].last_active_plot_tab = AppTab::Histogram;
                                }
                                KeyCode::Char('5') => {
                                    let old_tab = app_state.panes[ap].active_tab;
                                    if old_tab != AppTab::Settings {
                                        app_state.panes[ap].last_active_plot_tab = old_tab;
                                    }
                                    app_state.panes[ap].active_tab = AppTab::Settings;
                                    let _ = cmd_tx.send(consumer::WorkerCommand::GetSettings);
                                }
                                // ── Scope navigation (active pane) ───────────
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if app_state.panes[ap].active_tab == AppTab::Settings {
                                        if !app_state.settings.is_empty() {
                                            let cur = app_state.selected_setting_idx;
                                            app_state.selected_setting_idx = if cur == 0 {
                                                app_state.settings.len() - 1
                                            } else {
                                                cur - 1
                                            };
                                        }
                                    } else {
                                        if let ConnectionState::Connected { ref scopes, .. } =
                                            app_state.connection
                                        {
                                            if !scopes.is_empty() {
                                                let new_idx = app_state.panes[ap]
                                                    .selected_scope_idx
                                                    .checked_sub(1)
                                                    .unwrap_or(scopes.len() - 1);
                                                activate_scope(
                                                    scopes,
                                                    new_idx,
                                                    &mut app_state.panes[ap],
                                                );
                                                send_merged_scopes(
                                                    &app_state.panes,
                                                    app_state.num_panes,
                                                    &cmd_tx,
                                                );
                                            }
                                        }
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if app_state.panes[ap].active_tab == AppTab::Settings {
                                        if !app_state.settings.is_empty() {
                                            let cur = app_state.selected_setting_idx;
                                            app_state.selected_setting_idx =
                                                (cur + 1) % app_state.settings.len();
                                        }
                                    } else {
                                        if let ConnectionState::Connected { ref scopes, .. } =
                                            app_state.connection
                                        {
                                            if !scopes.is_empty() {
                                                let new_idx =
                                                    (app_state.panes[ap].selected_scope_idx + 1)
                                                        % scopes.len();
                                                activate_scope(
                                                    scopes,
                                                    new_idx,
                                                    &mut app_state.panes[ap],
                                                );
                                                send_merged_scopes(
                                                    &app_state.panes,
                                                    app_state.num_panes,
                                                    &cmd_tx,
                                                );
                                            }
                                        }
                                    }
                                }
                                KeyCode::Enter | KeyCode::Char(' ') => {
                                    if app_state.panes[ap].active_tab == AppTab::Settings {
                                        let idx = app_state.selected_setting_idx;
                                        if idx < app_state.settings.len() {
                                            let name = app_state.settings[idx].name.clone();
                                            if !app_state.pending_settings.contains(&name) {
                                                match &app_state.settings[idx].value {
                                                    consumer::SettingValue::Bool(b) => {
                                                        let new_val = !b;
                                                        app_state
                                                            .pending_settings
                                                            .insert(name.clone());
                                                        let _ = cmd_tx.send(consumer::WorkerCommand::UpdateSetting {
                                                            name,
                                                            setting_type: consumer::SettingType::Bool,
                                                            value: consumer::SettingValue::Bool(new_val),
                                                        });
                                                    }
                                                    consumer::SettingValue::Int32(i) => {
                                                        app_state.editing_setting_idx = Some(idx);
                                                        app_state.editing_setting_value =
                                                            i.to_string();
                                                    }
                                                    consumer::SettingValue::Float(f) => {
                                                        app_state.editing_setting_idx = Some(idx);
                                                        app_state.editing_setting_value =
                                                            f.to_string();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                // ── Collect / stacking (active pane) ─────────
                                KeyCode::Char('a') => {
                                    app_state.auto_collect_enabled =
                                        !app_state.auto_collect_enabled;
                                    let _ = cmd_tx.send(WorkerCommand::SetAutoCollect(
                                        app_state.auto_collect_enabled,
                                    ));
                                }
                                KeyCode::Char('s') => {
                                    app_state.panes[ap].stacking_enabled =
                                        !app_state.panes[ap].stacking_enabled;
                                }
                                KeyCode::Char('g') => {
                                    app_state.panes[ap].ungrouped = !app_state.panes[ap].ungrouped;
                                    if let ConnectionState::Connected { ref scopes, .. } =
                                        app_state.connection
                                    {
                                        if !scopes.is_empty() {
                                            let idx = app_state.panes[ap].selected_scope_idx;
                                            activate_scope(scopes, idx, &mut app_state.panes[ap]);
                                            send_merged_scopes(
                                                &app_state.panes,
                                                app_state.num_panes,
                                                &cmd_tx,
                                            );
                                        }
                                    }
                                }
                                KeyCode::Char('r') => {
                                    let _ = cmd_tx.send(WorkerCommand::RequestSingleFrame);
                                }
                                KeyCode::Char('R') => {
                                    app_state.status_message = "Refreshing scopes...".to_string();
                                    app_state.status_msg_time = Some(Instant::now());
                                    let _ = cmd_tx.send(WorkerCommand::RefreshScopes);
                                }
                                // ── Filter (active pane) ──────────────────────
                                KeyCode::Char('f') => {
                                    app_state.panes[ap].filter_enabled =
                                        !app_state.panes[ap].filter_enabled;
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.panes[ap].filter_enabled,
                                        cutoff: app_state.panes[ap].filter_cutoff,
                                        percentage: app_state.panes[ap].filter_percentage,
                                    });
                                }
                                KeyCode::Char('+') | KeyCode::Char('=') => {
                                    app_state.panes[ap].stacking_size =
                                        (app_state.panes[ap].stacking_size + 1000).min(100000);
                                    let sz = app_state.panes[ap].stacking_size;
                                    if let Some(ref mut s) = app_state.panes[ap].active_snapshot {
                                        s.max_stacked_size = sz;
                                    }
                                    for s in app_state.panes[ap].group_snapshots.values_mut() {
                                        s.max_stacked_size = sz;
                                    }
                                }
                                KeyCode::Char('-') | KeyCode::Char('_') => {
                                    app_state.panes[ap].stacking_size =
                                        (app_state.panes[ap].stacking_size.saturating_sub(1000))
                                            .max(1000);
                                    let sz = app_state.panes[ap].stacking_size;
                                    if let Some(ref mut s) = app_state.panes[ap].active_snapshot {
                                        s.max_stacked_size = sz;
                                    }
                                    for s in app_state.panes[ap].group_snapshots.values_mut() {
                                        s.max_stacked_size = sz;
                                    }
                                }
                                KeyCode::Char(']') => {
                                    app_state.panes[ap].filter_cutoff =
                                        (app_state.panes[ap].filter_cutoff + 10.0).min(32767.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.panes[ap].filter_enabled,
                                        cutoff: app_state.panes[ap].filter_cutoff,
                                        percentage: app_state.panes[ap].filter_percentage,
                                    });
                                }
                                KeyCode::Char('[') => {
                                    app_state.panes[ap].filter_cutoff =
                                        (app_state.panes[ap].filter_cutoff - 10.0).max(0.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.panes[ap].filter_enabled,
                                        cutoff: app_state.panes[ap].filter_cutoff,
                                        percentage: app_state.panes[ap].filter_percentage,
                                    });
                                }
                                KeyCode::Char('}') => {
                                    app_state.panes[ap].filter_percentage =
                                        (app_state.panes[ap].filter_percentage + 5.0).min(100.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.panes[ap].filter_enabled,
                                        cutoff: app_state.panes[ap].filter_cutoff,
                                        percentage: app_state.panes[ap].filter_percentage,
                                    });
                                }
                                KeyCode::Char('{') => {
                                    app_state.panes[ap].filter_percentage =
                                        (app_state.panes[ap].filter_percentage - 5.0).max(0.0);
                                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                                        enabled: app_state.panes[ap].filter_enabled,
                                        cutoff: app_state.panes[ap].filter_cutoff,
                                        percentage: app_state.panes[ap].filter_percentage,
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

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

// ── Draw ───────────────────────────────────────────────────────────────────────

fn draw_ui(frame: &mut Frame, state: &mut AppState) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Header
            Constraint::Min(10),   // Content
            Constraint::Length(2), // Help (shortcuts)
            Constraint::Length(1), // Message Log
        ])
        .split(frame.area());

    // Header
    let fps_text = format!("FPS: {:.1}", state.current_fps);
    let pane_info = match state.num_panes {
        0 => " | Plots: 0".to_string(),
        1 => " | Plots: 1".to_string(),
        _ => format!(" | Plots: 2  Active: P{}", state.active_pane + 1),
    };
    let title_block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Cyan));
    let title_paragraph = Paragraph::new(Line::from(vec![
        Span::raw(" 🛠️  ").fg(Color::Cyan),
        Span::styled(
            "IMScope TUI - Real-time Signal Analyzer",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        ),
        Span::raw(" | ").fg(Color::DarkGray),
        Span::styled(fps_text, Style::default().fg(Color::Green)),
        Span::styled(pane_info, Style::default().fg(Color::Cyan)),
    ]))
    .block(title_block)
    .alignment(Alignment::Left);
    frame.render_widget(title_paragraph, main_chunks[0]);

    // Content: sidebar + plot area(s) + settings pane
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(42), // Sidebar
            Constraint::Min(20),    // Plot Area
            Constraint::Length(48), // Settings Pane
        ])
        .split(main_chunks[1]);

    draw_sidebar(frame, content_chunks[0], state);

    match state.num_panes {
        0 => {
            let msg = Paragraph::new("\n\nNo plots active.\nPress 'P' to add a plot.")
                .alignment(Alignment::Center)
                .fg(Color::DarkGray)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
            frame.render_widget(msg, content_chunks[1]);
        }
        1 => {
            draw_plot_area(frame, content_chunks[1], state, 0);
        }
        _ => {
            let plot_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(content_chunks[1]);
            draw_plot_area(frame, plot_chunks[0], state, 0);
            draw_plot_area(frame, plot_chunks[1], state, 1);
        }
    }

    draw_settings_pane(frame, content_chunks[2], state);

    // Help bar
    let shortcut_text = "q:Quit i:URL c:Connect R:Refresh a:Auto r:Single s:Stack g:Ungroup f:Filter P:Panes p:Switch";
    let help_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            " [KEYS] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {} ", shortcut_text)),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(help_paragraph, main_chunks[2]);

    // Message Log bar
    let log_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            " [LOG] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}", state.status_message),
            Style::default().fg(Color::Yellow),
        ),
    ]))
    .block(Block::default().borders(Borders::NONE));
    frame.render_widget(log_paragraph, main_chunks[3]);
}

fn draw_sidebar(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Min(10),
        ])
        .split(area);

    // ── 1. Connection block ────────────────────────────────────────────────────
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

    let pane_label = match state.num_panes {
        0 => "No plots  (P: add)".to_string(),
        1 => "1 plot  (P: add, p: -)".to_string(),
        _ => format!("2 plots  Active: P{}  (p: switch)", state.active_pane + 1),
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
        Line::from(vec![Span::styled(
            pane_label,
            Style::default().fg(Color::Cyan),
        )]),
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

    // ── 2. Scopes list ─────────────────────────────────────────────────────────
    let ap = state.active_pane;
    let mut list_items = Vec::new();
    let mut active_scope_name = "None".to_string();
    let mut active_scope_type = ScopeType::IqData;

    if let ConnectionState::Connected { ref scopes, .. } = state.connection {
        let active_group = scopes
            .get(state.panes[ap].selected_scope_idx)
            .map(|s| s.group.as_str())
            .unwrap_or("");

        // Inactive pane's selection (when 2 panes shown)
        let inactive_sel = if state.num_panes == 2 {
            Some(state.panes[1 - ap].selected_scope_idx)
        } else {
            None
        };
        let inactive_group = inactive_sel
            .and_then(|idx| scopes.get(idx))
            .map(|s| s.group.as_str())
            .unwrap_or("");

        for (i, scope) in scopes.iter().enumerate() {
            let type_str = match scope.scope_type {
                ScopeType::Real => "Real",
                ScopeType::IqData => "IQ",
                ScopeType::Int32 => "Int32",
                ScopeType::Float => "Float",
            };
            let item_text = if !scope.group.is_empty() {
                format!("{:02}. [{}] {} [{}]", i, scope.group, scope.name, type_str)
            } else {
                format!("{:02}. {} [{}]", i, scope.name, type_str)
            };

            let is_active_sel = i == state.panes[ap].selected_scope_idx;
            let is_inactive_sel = inactive_sel == Some(i);
            let is_active_grp = !scope.group.is_empty()
                && !active_group.is_empty()
                && scope.group == active_group
                && !is_active_sel;
            let is_inactive_grp = !scope.group.is_empty()
                && !inactive_group.is_empty()
                && scope.group == inactive_group
                && !is_inactive_sel
                && !is_active_sel;

            let style = if is_active_sel {
                active_scope_name = scope.name.clone();
                active_scope_type = scope.scope_type;
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_inactive_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else if is_active_grp {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_inactive_grp {
                Style::default()
                    .fg(Color::Yellow)
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
    frame.render_stateful_widget(list, chunks[1], &mut state.panes[ap].scope_list_state);

    // ── 3. Settings (active pane) ──────────────────────────────────────────────
    let pane = &state.panes[ap];

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

    let stacking_status = if pane.stacking_enabled {
        Span::styled(
            " [x] ENABLED",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [ ] DISABLED", Style::default().fg(Color::DarkGray))
    };

    let filter_status = if pane.filter_enabled {
        Span::styled(
            " [x] ENABLED",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [ ] DISABLED", Style::default().fg(Color::DarkGray))
    };

    let mut settings_lines = Vec::new();

    if pane.active_tab == AppTab::Settings {
        settings_lines.push(Line::from(vec![Span::styled(
            "Dynamic Settings Controls:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]));
        settings_lines.push(Line::from(vec![
            Span::raw("  ▲/▼ or j/k : "),
            Span::styled("Select setting", Style::default().fg(Color::Cyan)),
        ]));
        settings_lines.push(Line::from(vec![
            Span::raw("  Enter/Space: "),
            Span::styled("Toggle / Edit value", Style::default().fg(Color::Cyan)),
        ]));
        settings_lines.push(Line::from(vec![
            Span::raw("  Esc        : "),
            Span::styled("Cancel edit", Style::default().fg(Color::Cyan)),
        ]));
        settings_lines.push(Line::from(Span::raw("")));
        settings_lines.push(Line::from(vec![
            Span::styled(
                "Auto Collect ('a'):",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            auto_status,
        ]));
    } else {
        settings_lines.push(Line::from(vec![
            Span::styled(
                "Auto Collect ('a'):",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            auto_status,
        ]));
        settings_lines.push(Line::from(vec![
            Span::styled(
                "Stacking ('s'):    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            stacking_status,
        ]));
        settings_lines.push(Line::from(vec![
            Span::raw("Stacking size: "),
            Span::styled(
                format!("{:>5}", pane.stacking_size),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(" [-] ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [+] ", Style::default().fg(Color::White).bg(Color::Green)),
        ]));
        settings_lines.push(Line::from(vec![
            Span::styled(
                "Ungroup ('g'):     ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            if pane.ungrouped {
                Span::styled(
                    " [x] INDIVIDUAL",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" [ ] GROUPED", Style::default().fg(Color::DarkGray))
            },
        ]));
        settings_lines.push(Line::from(Span::raw("")));
        settings_lines.push(Line::from(vec![
            Span::styled(
                "Noise Filter ('f'):",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            filter_status,
        ]));
        settings_lines.push(Line::from(vec![
            Span::raw("Cutoff linear: "),
            Span::styled(
                format!("{:>5.0}", pane.filter_cutoff),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(" [-] ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [+] ", Style::default().fg(Color::White).bg(Color::Green)),
        ]));
        settings_lines.push(Line::from(vec![
            Span::raw("Max noise %:   "),
            Span::styled(
                format!("{:>4.0}%", pane.filter_percentage),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(" [-] ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [+] ", Style::default().fg(Color::White).bg(Color::Green)),
        ]));
        settings_lines.push(Line::from(Span::raw("")));
        settings_lines.push(Line::from(vec![Span::styled(
            "Active Scope Info:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::UNDERLINED),
        )]));
        settings_lines.push(Line::from(vec![
            Span::raw("Name: "),
            Span::styled(active_scope_name, Style::default().fg(Color::Green)),
        ]));
        settings_lines.push(Line::from(vec![
            Span::raw("Type: "),
            Span::styled(
                format!("{:?}", active_scope_type),
                Style::default().fg(Color::Green),
            ),
        ]));

        if let Some(ref snapshot) = pane.active_snapshot {
            settings_lines.push(Line::from(vec![
                Span::raw("BufferSize: "),
                Span::styled(
                    format!("{}", snapshot.size()),
                    Style::default().fg(Color::Green),
                ),
            ]));
        } else if pane.in_group_mode && !pane.group_snapshots.is_empty() {
            let total: usize = pane.group_snapshots.values().map(|s| s.size()).sum();
            settings_lines.push(Line::from(vec![
                Span::raw("BufferSize: "),
                Span::styled(
                    format!("{} (group)", total),
                    Style::default().fg(Color::Cyan),
                ),
            ]));
        }
    }

    let settings_block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Controls & Settings (P{}) ", ap + 1))
        .border_style(Style::default().fg(Color::DarkGray));

    frame.render_widget(
        Paragraph::new(settings_lines).block(settings_block),
        chunks[2],
    );
}

fn draw_settings_pane(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let ap = state.active_pane;
    let is_settings_focused = state.num_panes > 0 && state.panes[ap].active_tab == AppTab::Settings;
    let border_color = if is_settings_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Settings [5] ")
        .border_style(Style::default().fg(border_color));

    if state.settings.is_empty() {
        let msg = Paragraph::new(
            "\n\nNo dynamic settings\nregistered by producer.\n\nOr producer is not connected.",
        )
        .alignment(Alignment::Center)
        .fg(Color::DarkGray)
        .block(block);
        frame.render_widget(msg, area);
        return;
    }

    // Split the settings pane area vertically to have the table and a footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(5)])
        .split(area);

    let mut rows = Vec::new();
    for (idx, setting) in state.settings.iter().enumerate() {
        let is_selected = is_settings_focused && idx == state.selected_setting_idx;
        let is_pending = state.pending_settings.contains(&setting.name);
        let is_editing = is_settings_focused && state.editing_setting_idx == Some(idx);

        let selector = if is_selected { "➔ " } else { "  " };

        let row_style = if is_pending {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC)
        } else if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let name_cell = ratatui::widgets::Cell::from(setting.name.as_str());
        let type_cell = ratatui::widgets::Cell::from(match setting.setting_type {
            consumer::SettingType::Bool => "Bool",
            consumer::SettingType::Int32 => "Int32",
            consumer::SettingType::Float => "Float",
        });

        let val_str = if is_editing {
            format!("✍  {}█", state.editing_setting_value)
        } else if is_pending {
            match &setting.value {
                consumer::SettingValue::Bool(b) => format!("Updating to {}...", !b),
                _ => "Updating...".to_string(),
            }
        } else {
            match &setting.value {
                consumer::SettingValue::Bool(b) => b.to_string(),
                consumer::SettingValue::Int32(i) => i.to_string(),
                consumer::SettingValue::Float(f) => format!("{:.4}", f),
            }
        };

        let val_cell = ratatui::widgets::Cell::from(val_str);

        rows.push(
            ratatui::widgets::Row::new(vec![
                ratatui::widgets::Cell::from(selector),
                name_cell,
                type_cell,
                val_cell,
            ])
            .style(row_style),
        );
    }

    let table = ratatui::widgets::Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Percentage(40),
            Constraint::Percentage(20),
            Constraint::Percentage(40),
        ],
    )
    .header(
        ratatui::widgets::Row::new(vec![
            ratatui::widgets::Cell::from(""),
            ratatui::widgets::Cell::from("Setting Name"),
            ratatui::widgets::Cell::from("Type"),
            ratatui::widgets::Cell::from("Value / Input"),
        ])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(block);

    frame.render_widget(table, chunks[0]);

    // Render helper/metadata for settings in the footer
    let meta_block = Block::default()
        .borders(Borders::ALL)
        .title(" Settings Help & Status ")
        .border_style(Style::default().fg(border_color));

    let meta_lines = vec![
        Line::from(vec![
            Span::raw("Keys: "),
            Span::styled("▲/▼", Style::default().fg(Color::Cyan)),
            Span::raw(" Select  "),
            Span::styled("Enter/Space", Style::default().fg(Color::Cyan)),
            Span::raw(" Edit/Toggle"),
        ]),
        Line::from(vec![
            Span::raw("Status: "),
            if state.pending_settings.is_empty() {
                Span::styled("All updates confirmed.", Style::default().fg(Color::Green))
            } else {
                Span::styled(
                    format!(
                        "Awaiting {} confirmation(s)...",
                        state.pending_settings.len()
                    ),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC),
                )
            },
        ]),
    ];
    frame.render_widget(Paragraph::new(meta_lines).block(meta_block), chunks[1]);
}

fn draw_plot_area(frame: &mut Frame, area: Rect, state: &mut AppState, pane_idx: usize) {
    let is_active = pane_idx == state.active_pane;
    let border_color = if is_active { Color::Cyan } else { Color::Blue };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
        ])
        .split(area);

    // Determine scope type for the current pane's selection
    let active_scope_type_for_meta =
        if let ConnectionState::Connected { ref scopes, .. } = state.connection {
            scopes
                .get(state.panes[pane_idx].selected_scope_idx)
                .map(|s| s.scope_type)
        } else {
            None
        };

    // ── Tab bar ────────────────────────────────────────────────────────────────
    let tab_titles = vec![
        "1. Scatter (IQ only)",
        "2. RMS Power (IQ only)",
        "3. Waveform",
        "4. Histogram",
        "5. Settings",
    ];
    let tab_style = Style::default().fg(Color::White);
    let selected_style = Style::default()
        .fg(Color::Black)
        .bg(border_color)
        .add_modifier(Modifier::BOLD);

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Plot P{} (Tab) ", pane_idx + 1))
                .border_style(Style::default().fg(border_color)),
        )
        .select(state.panes[pane_idx].active_tab.as_index())
        .style(tab_style)
        .highlight_style(selected_style);
    frame.render_widget(tabs, chunks[0]);

    // ── Plot block ─────────────────────────────────────────────────────────────
    let plot_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let pane = &state.panes[pane_idx];
    let display_tab = if pane.active_tab == AppTab::Settings {
        pane.last_active_plot_tab
    } else {
        pane.active_tab
    };

    if pane.in_group_mode {
        // ── Group mode ──────────────────────────────────────────────────────────
        let mut members: Vec<(String, Vec<f64>, f64, f64)> = Vec::new();
        if let ConnectionState::Connected { ref scopes, .. } = state.connection {
            let mut ids: Vec<usize> = pane.group_snapshots.keys().copied().collect();
            ids.sort();
            for id in ids {
                if let Some(snap) = pane.group_snapshots.get(&id) {
                    if snap.size() > 0 {
                        members.push((
                            scopes[id].name.clone(),
                            snap.real.clone(),
                            snap.min_val,
                            snap.max_val,
                        ));
                    }
                }
            }
        }

        match display_tab {
            AppTab::Scatter | AppTab::Rms => {
                let msg = Paragraph::new(
                    "\n\nScatter and RMS Power plots are not available for grouped scopes.",
                )
                .alignment(Alignment::Center)
                .fg(Color::Yellow)
                .block(plot_block);
                frame.render_widget(msg, chunks[1]);
            }
            AppTab::Waveform | AppTab::Histogram => {
                if members.is_empty() {
                    let msg = Paragraph::new(
                        "\n\nNo group data received yet.\nEnable 'Auto Collect' or press 'r'.",
                    )
                    .alignment(Alignment::Center)
                    .fg(Color::DarkGray)
                    .block(plot_block);
                    frame.render_widget(msg, chunks[1]);
                } else if pane.active_tab == AppTab::Waveform {
                    let global_min = members
                        .iter()
                        .map(|(_, _, lo, _)| *lo)
                        .fold(f64::MAX, f64::min);
                    let global_max = members
                        .iter()
                        .map(|(_, _, _, hi)| *hi)
                        .fold(f64::MIN, f64::max);
                    let max_samples = members
                        .iter()
                        .map(|(_, d, _, _)| d.len())
                        .max()
                        .unwrap_or(1);
                    let margin = ((global_max - global_min) * 0.1).max(1.0);
                    let y_lo = global_min - margin;
                    let y_hi = global_max + margin;

                    let canvas = Canvas::default()
                        .block(plot_block.title(" Group Values (Time Series) "))
                        .x_bounds([0.0, max_samples as f64])
                        .y_bounds([y_lo, y_hi])
                        .paint(move |ctx| {
                            let baseline = 0.0_f64.clamp(y_lo, y_hi);
                            ctx.draw(&ratatui::widgets::canvas::Line {
                                x1: 0.0,
                                y1: baseline,
                                x2: max_samples as f64,
                                y2: baseline,
                                color: Color::DarkGray,
                            });
                            for (i, (name, data, _, _)) in members.iter().enumerate() {
                                let color = GROUP_COLORS[i % GROUP_COLORS.len()];
                                let n = data.len();
                                let step = (n / 500).max(1);
                                for j in (0..n.saturating_sub(step)).step_by(step) {
                                    ctx.draw(&ratatui::widgets::canvas::Line {
                                        x1: j as f64,
                                        y1: data[j],
                                        x2: (j + step) as f64,
                                        y2: data[j + step],
                                        color,
                                    });
                                }
                                let legend_y =
                                    y_hi - (i as f64 + 1.0) * ((y_hi - y_lo) * 0.08).max(0.5);
                                ctx.print(
                                    max_samples as f64 * 0.01,
                                    legend_y,
                                    format!("■ {}", name).fg(color),
                                );
                            }
                        });
                    frame.render_widget(canvas, chunks[1]);
                } else {
                    // Histogram per group member
                    let num_bins = 16usize;
                    let member_count = members.len();
                    let bar_width = ((chunks[1].width as usize).saturating_sub(2))
                        / (member_count * (num_bins + 1)).max(1);
                    let bar_width = bar_width.max(1) as u16;

                    let mut all_bars: Vec<Bar> = Vec::new();
                    for (m_idx, (name, data, min_v, max_v)) in members.iter().enumerate() {
                        let color = GROUP_COLORS[m_idx % GROUP_COLORS.len()];
                        let lo = *min_v as f32;
                        let hi = (*max_v as f32).max(lo + 1.0);
                        let bin_w = (hi - lo) / num_bins as f32;
                        let mut counts = vec![0u64; num_bins];
                        for &v in data {
                            let idx = (((v as f32 - lo) / bin_w) as usize).min(num_bins - 1);
                            counts[idx] += 1;
                        }
                        if m_idx > 0 {
                            all_bars.push(Bar::default().value(0).label(""));
                        }
                        for (b, &cnt) in counts.iter().enumerate() {
                            let center = lo + (b as f32 + 0.5) * bin_w;
                            all_bars.push(
                                Bar::default()
                                    .value(cnt)
                                    .label(if b == 0 { name.as_str() } else { "" })
                                    .style(Style::default().fg(color))
                                    .value_style(Style::default().fg(Color::White))
                                    .text_value(format!("{:.0}", center)),
                            );
                        }
                    }
                    let group_data = BarGroup::default().bars(&all_bars);
                    let chart = BarChart::default()
                        .block(plot_block.title(" Group Histogram "))
                        .data(group_data)
                        .bar_width(bar_width)
                        .bar_gap(0)
                        .label_style(Style::default().fg(Color::Gray));
                    frame.render_widget(chart, chunks[1]);
                }
            }
            AppTab::Settings => unreachable!(),
        }
    } else if let Some(ref snapshot) = pane.active_snapshot {
        if snapshot.size() == 0 {
            let msg = Paragraph::new(
                "\n\nNo scope data received yet.\nEnable 'Auto Collect' or press 'r'.",
            )
            .alignment(Alignment::Center)
            .fg(Color::DarkGray)
            .block(plot_block);
            frame.render_widget(msg, chunks[1]);
        } else {
            match display_tab {
                AppTab::Scatter => {
                    if let ConnectionState::Connected { ref scopes, .. } = state.connection {
                        if scopes[pane.selected_scope_idx].scope_type != ScopeType::IqData {
                            let msg = Paragraph::new(
                                "\n\nScatter/Constellation plot is only available for IQ scopes.",
                            )
                            .alignment(Alignment::Center)
                            .fg(Color::Yellow)
                            .block(plot_block);
                            frame.render_widget(msg, chunks[1]);
                            // still render metadata below
                        } else {
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
                                .block(
                                    plot_block.title(" Constellation Scatterplot (Imag vs Real) "),
                                )
                                .x_bounds([-limit, limit])
                                .y_bounds([-limit, limit])
                                .paint(move |ctx| {
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
                                    ctx.draw(&Points {
                                        coords: &points,
                                        color: Color::Cyan,
                                    });
                                    ctx.print(
                                        -limit * 0.95,
                                        limit * 0.85,
                                        format!("Max IQ: {:.0}", limit).fg(Color::Gray),
                                    );
                                });
                            frame.render_widget(canvas, chunks[1]);
                        }
                    }
                }
                AppTab::Rms => {
                    if let ConnectionState::Connected { ref scopes, .. } = state.connection {
                        if scopes[pane.selected_scope_idx].scope_type != ScopeType::IqData {
                            let msg = Paragraph::new(
                                "\n\nRMS Power plot is only available for IQ scopes.",
                            )
                            .alignment(Alignment::Center)
                            .fg(Color::Yellow)
                            .block(plot_block);
                            frame.render_widget(msg, chunks[1]);
                        } else {
                            let max_p = (snapshot.max_power as f64 * 1.1).max(1.0);
                            let num_samples = snapshot.power.len();
                            let canvas = Canvas::default()
                                .block(plot_block.title(" RMS Power over Samples (r^2 + im^2) "))
                                .x_bounds([0.0, num_samples as f64])
                                .y_bounds([0.0, max_p])
                                .paint(move |ctx| {
                                    ctx.draw(&ratatui::widgets::canvas::Line {
                                        x1: 0.0,
                                        y1: max_p * 0.5,
                                        x2: num_samples as f64,
                                        y2: max_p * 0.5,
                                        color: Color::Indexed(236),
                                    });
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
                                    ctx.print(
                                        0.0,
                                        max_p * 0.9,
                                        format!("Max Power: {:.0}", max_p).fg(Color::Gray),
                                    );
                                });
                            frame.render_widget(canvas, chunks[1]);
                        }
                    }
                }
                AppTab::Waveform => {
                    let is_scalar = matches!(
                        active_scope_type_for_meta,
                        Some(ScopeType::Int32) | Some(ScopeType::Float)
                    );
                    let num_samples = snapshot.real.len();
                    let (y_lo, y_hi, title) = if is_scalar {
                        let margin = ((snapshot.max_val - snapshot.min_val) * 0.1).max(1.0);
                        (
                            snapshot.min_val - margin,
                            snapshot.max_val + margin,
                            " Values (Time Series) ",
                        )
                    } else {
                        let limit = (snapshot.max_iq * 1.1).max(1.0);
                        (-limit, limit, " Real Amplitudes Waveform ")
                    };
                    let canvas = Canvas::default()
                        .block(plot_block.title(title))
                        .x_bounds([0.0, num_samples as f64])
                        .y_bounds([y_lo, y_hi])
                        .paint(move |ctx| {
                            let baseline = 0.0_f64.clamp(y_lo, y_hi);
                            ctx.draw(&ratatui::widgets::canvas::Line {
                                x1: 0.0,
                                y1: baseline,
                                x2: num_samples as f64,
                                y2: baseline,
                                color: Color::DarkGray,
                            });
                            let step = (num_samples / 500).max(1);
                            for i in (0..num_samples.saturating_sub(step)).step_by(step) {
                                ctx.draw(&ratatui::widgets::canvas::Line {
                                    x1: i as f64,
                                    y1: snapshot.real[i],
                                    x2: (i + step) as f64,
                                    y2: snapshot.real[i + step],
                                    color: Color::Green,
                                });
                            }
                            ctx.print(
                                0.0,
                                y_hi * 0.9,
                                format!("Range: [{:.1}, {:.1}]", y_lo, y_hi).fg(Color::Gray),
                            );
                        });
                    frame.render_widget(canvas, chunks[1]);
                }
                AppTab::Histogram => {
                    let is_scalar = matches!(
                        active_scope_type_for_meta,
                        Some(ScopeType::Int32) | Some(ScopeType::Float)
                    );
                    let num_bins = 20;
                    let mut bin_counts = vec![0u64; num_bins];
                    let (range_lo, range_hi) = if is_scalar {
                        let lo = snapshot.min_val as f32;
                        let hi = (snapshot.max_val as f32).max(lo + 1.0);
                        (lo, hi)
                    } else {
                        let limit = snapshot.max_iq.max(1.0) as f32;
                        (-limit, limit)
                    };
                    let bin_width = (range_hi - range_lo) / (num_bins as f32);
                    for &val in &snapshot.real {
                        let idx =
                            (((val as f32 - range_lo) / bin_width) as usize).min(num_bins - 1);
                        bin_counts[idx] += 1;
                    }
                    let bars: Vec<Bar> = (0..num_bins)
                        .map(|i| {
                            let center = range_lo + (i as f32 + 0.5) * bin_width;
                            Bar::default()
                                .label(format!("{:.0}", center))
                                .value(bin_counts[i])
                                .style(Style::default().fg(Color::Magenta))
                        })
                        .collect();
                    let chart = BarChart::default()
                        .block(
                            plot_block
                                .title(" Amplitude Density Distribution (1D Real Histogram) "),
                        )
                        .data(BarGroup::default().bars(&bars))
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
                AppTab::Settings => unreachable!(),
            }
        }
    } else {
        let msg = Paragraph::new(
            "\n\nNot connected to any producer.\nPlease enter announcer address on the left and connect.",
        )
        .alignment(Alignment::Center)
        .fg(Color::DarkGray)
        .block(plot_block);
        frame.render_widget(msg, chunks[1]);
    }

    // ── Metadata footer ────────────────────────────────────────────────────────
    let meta_block = Block::default()
        .borders(Borders::ALL)
        .title(" Active Frame Metadata ")
        .border_style(Style::default().fg(Color::DarkGray));

    let pane = &state.panes[pane_idx];
    if let Some(ref snapshot) = pane.active_snapshot {
        let is_scalar = matches!(
            active_scope_type_for_meta,
            Some(ScopeType::Int32) | Some(ScopeType::Float)
        );
        let first_line = if is_scalar {
            Line::from(vec![
                Span::raw("Slot: "),
                Span::styled("N/A", Style::default().fg(Color::DarkGray)),
                Span::raw("   Frame: "),
                Span::styled("N/A", Style::default().fg(Color::DarkGray)),
                Span::raw("   Timestamp: "),
                Span::styled("N/A", Style::default().fg(Color::DarkGray)),
            ])
        } else {
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
            ])
        };
        let max_label = if is_scalar {
            "Max absolute value: "
        } else {
            "Max absolute IQ value: "
        };
        let meta_lines = vec![
            first_line,
            Line::from(vec![
                Span::raw(max_label),
                Span::styled(
                    format!("{}", snapshot.max_iq),
                    Style::default().fg(Color::Green),
                ),
                Span::raw("   Non-zero samples: "),
                Span::styled(
                    format!("{}", snapshot.nonzero_count),
                    Style::default().fg(Color::Green),
                ),
                Span::raw("   Total stacked: "),
                Span::styled(
                    format!("{}", snapshot.size()),
                    Style::default().fg(Color::Green),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(meta_lines).block(meta_block), chunks[2]);
    } else if pane.in_group_mode {
        let group_name = if let ConnectionState::Connected { ref scopes, .. } = state.connection {
            scopes
                .get(pane.selected_scope_idx)
                .map(|s| s.group.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let mut meta_lines = vec![Line::from(vec![
            Span::raw("Group: "),
            Span::styled(
                group_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   Slot/Frame/Timestamp: "),
            Span::styled("N/A", Style::default().fg(Color::DarkGray)),
        ])];

        if let ConnectionState::Connected { ref scopes, .. } = state.connection {
            let mut ids: Vec<usize> = pane.group_snapshots.keys().copied().collect();
            ids.sort();
            let mut parts: Vec<Span> = vec![Span::raw("Members: ")];
            for (k, &id) in ids.iter().enumerate() {
                if let (Some(snap), Some(scope)) = (pane.group_snapshots.get(&id), scopes.get(id)) {
                    if k > 0 {
                        parts.push(Span::raw("   "));
                    }
                    let color = GROUP_COLORS[k % GROUP_COLORS.len()];
                    parts.push(Span::styled(
                        format!("■ {}: {} samples", scope.name, snap.size()),
                        Style::default().fg(color),
                    ));
                }
            }
            meta_lines.push(Line::from(parts));
        }
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

// ── Mouse click handler ────────────────────────────────────────────────────────

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
            Constraint::Length(2), // Header
            Constraint::Min(10),   // Content
            Constraint::Length(2), // Help
            Constraint::Length(1), // Log
        ])
        .split(area);

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(42), // Sidebar
            Constraint::Min(20),    // Plot Area
            Constraint::Length(48), // Settings Pane
        ])
        .split(main_chunks[1]);

    let sidebar_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Min(10),
        ])
        .split(content_chunks[0]);

    let plot_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
        ])
        .split(content_chunks[1]);

    let ap = state.active_pane;

    // 1. Connection block
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

    // 2. Scopes list
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
                    activate_scope(scopes, clicked_idx, &mut state.panes[ap]);
                    send_merged_scopes(&state.panes, state.num_panes, cmd_tx);
                }
            }
        }
        return;
    }

    // 3. Controls settings
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
                state.panes[ap].stacking_enabled = !state.panes[ap].stacking_enabled;
            }
            2 => {
                let relative_col = col as i32 - controls.x as i32 - 1;
                if relative_col >= 22 && relative_col <= 26 {
                    state.panes[ap].stacking_size =
                        (state.panes[ap].stacking_size.saturating_sub(1000)).max(1000);
                    let sz = state.panes[ap].stacking_size;
                    if let Some(ref mut s) = state.panes[ap].active_snapshot {
                        s.max_stacked_size = sz;
                    }
                } else if relative_col >= 28 && relative_col <= 32 {
                    state.panes[ap].stacking_size =
                        (state.panes[ap].stacking_size + 1000).min(100000);
                    let sz = state.panes[ap].stacking_size;
                    if let Some(ref mut s) = state.panes[ap].active_snapshot {
                        s.max_stacked_size = sz;
                    }
                }
            }
            4 => {
                state.panes[ap].filter_enabled = !state.panes[ap].filter_enabled;
                let _ = cmd_tx.send(WorkerCommand::SetFilter {
                    enabled: state.panes[ap].filter_enabled,
                    cutoff: state.panes[ap].filter_cutoff,
                    percentage: state.panes[ap].filter_percentage,
                });
            }
            5 => {
                let relative_col = col as i32 - controls.x as i32 - 1;
                if relative_col >= 22 && relative_col <= 26 {
                    state.panes[ap].filter_cutoff = (state.panes[ap].filter_cutoff - 10.0).max(0.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.panes[ap].filter_enabled,
                        cutoff: state.panes[ap].filter_cutoff,
                        percentage: state.panes[ap].filter_percentage,
                    });
                } else if relative_col >= 28 && relative_col <= 32 {
                    state.panes[ap].filter_cutoff =
                        (state.panes[ap].filter_cutoff + 10.0).min(32767.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.panes[ap].filter_enabled,
                        cutoff: state.panes[ap].filter_cutoff,
                        percentage: state.panes[ap].filter_percentage,
                    });
                }
            }
            6 => {
                let relative_col = col as i32 - controls.x as i32 - 1;
                if relative_col >= 22 && relative_col <= 26 {
                    state.panes[ap].filter_percentage =
                        (state.panes[ap].filter_percentage - 5.0).max(0.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.panes[ap].filter_enabled,
                        cutoff: state.panes[ap].filter_cutoff,
                        percentage: state.panes[ap].filter_percentage,
                    });
                } else if relative_col >= 28 && relative_col <= 32 {
                    state.panes[ap].filter_percentage =
                        (state.panes[ap].filter_percentage + 5.0).min(100.0);
                    let _ = cmd_tx.send(WorkerCommand::SetFilter {
                        enabled: state.panes[ap].filter_enabled,
                        cutoff: state.panes[ap].filter_cutoff,
                        percentage: state.panes[ap].filter_percentage,
                    });
                }
            }
            _ => {}
        }
        return;
    }

    // 4. Tab bar (first plot pane when split, or single pane)
    let tab_bar = plot_chunks[0];
    if col >= tab_bar.x
        && col < tab_bar.x + tab_bar.width
        && row >= tab_bar.y
        && row < tab_bar.y + tab_bar.height
    {
        let relative_col = col as i32 - tab_bar.x as i32 - 1;
        if relative_col >= 0 {
            let next_tab = if relative_col < 20 {
                AppTab::Scatter
            } else if relative_col < 42 {
                AppTab::Rms
            } else if relative_col < 54 {
                AppTab::Waveform
            } else if relative_col < 67 {
                AppTab::Histogram
            } else {
                AppTab::Settings
            };
            if next_tab != AppTab::Settings {
                state.panes[ap].last_active_plot_tab = next_tab;
            }
            state.panes[ap].active_tab = next_tab;
            if next_tab == AppTab::Settings {
                let _ = cmd_tx.send(WorkerCommand::GetSettings);
            }
        }
        return;
    }

    // 5. Settings pane click (focus settings)
    let settings_rect = content_chunks[2];
    if col >= settings_rect.x
        && col < settings_rect.x + settings_rect.width
        && row >= settings_rect.y
        && row < settings_rect.y + settings_rect.height
    {
        state.panes[ap].active_tab = AppTab::Settings;
        let _ = cmd_tx.send(WorkerCommand::GetSettings);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_mouse_click_tabs() {
        let mut state = AppState::new("tcp://127.0.0.1:5557".to_string());
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 180, 24);

        // tab_bar: x=42, y=2, width=90, height=3
        // Waveform is relative_col=45, so col=42+1+45=88, row=2+1=3
        handle_mouse_click(88, 3, area, &mut state, &tx);
        assert_eq!(state.panes[0].active_tab, AppTab::Waveform);
    }

    #[test]
    fn test_handle_mouse_click_refresh() {
        let mut state = AppState::new("tcp://127.0.0.1:5557".to_string());
        let (tx, rx) = mpsc::channel();
        let area = Rect::new(0, 0, 120, 35);

        // Header height is 2, so Content starts at y=2.
        // Sidebar connection block conn is y=2, height=6.
        // Refresh scopes is at conn.y + 4 = 6.
        handle_mouse_click(5, 6, area, &mut state, &tx);

        let cmd = rx.try_recv().unwrap();
        match cmd {
            WorkerCommand::RefreshScopes => {}
            _ => panic!("Expected RefreshScopes command"),
        }
        assert_eq!(state.status_message, "Refreshing scopes...");
    }
}
