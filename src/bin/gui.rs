use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use dear_app::{AddOnsConfig, AppBuilder, Theme};
use dear_imgui_rs::{Condition, TreeNodeFlags};
use dear_implot::*;

use imscope::app::{ConnectionState, PlotPane, activate_scope, send_merged_scopes};
use imscope::consumer::{
    self as consumer, ScopeType, SettingValue, WorkerCommand, WorkerEvent, run_worker,
};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "GUI client for imscope using Dear ImGui + ImPlot"
)]
struct Args {
    #[arg(short, long, default_value = "tcp://127.0.0.1:5557")]
    announce_url: String,
}

struct AppState {
    connection: ConnectionState,
    announce_url: String,
    panes: [PlotPane; 2],
    num_panes: usize,
    auto_collect_enabled: bool,
    frame_rate_counter: u32,
    last_frame_rate_calc: Instant,
    current_fps: f32,
    status_message: String,
    status_log: Vec<String>,
    settings: Vec<consumer::SettingInfo>,
    gui_settings: HashMap<String, SettingValue>,
}

impl AppState {
    fn new(announce_url: String) -> Self {
        Self {
            connection: ConnectionState::Disconnected(None),
            announce_url,
            panes: [PlotPane::new(), PlotPane::new()],
            num_panes: 1,
            auto_collect_enabled: true,
            frame_rate_counter: 0,
            last_frame_rate_calc: Instant::now(),
            current_fps: 0.0,
            status_message: String::new(),
            status_log: Vec::new(),
            settings: Vec::new(),
            gui_settings: HashMap::new(),
        }
    }

    fn update_status(&mut self, msg: String) {
        self.status_message = msg.clone();
        self.status_log.push(msg);
        if self.status_log.len() > 100 {
            self.status_log.remove(0);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();

    let cmd_tx_clone = cmd_tx.clone();
    thread::spawn(move || {
        run_worker(cmd_rx, event_tx);
    });

    let mut app_state = AppState::new(args.announce_url.clone());
    app_state.update_status(format!("Connecting to {}...", args.announce_url));
    let _ = cmd_tx.send(WorkerCommand::Connect {
        url: args.announce_url.clone(),
    });

    AppBuilder::new()
        .with_addons(AddOnsConfig::auto())
        .with_theme(Theme::Dark)
        .on_frame(move |ui, addons| {
            // ─── Process Worker Events ───
            while let Ok(event) = event_rx.try_recv() {
                match event {
                    WorkerEvent::Connecting => {
                        app_state.connection = ConnectionState::Connecting;
                        app_state.update_status("Connecting to announcer...".to_string());
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
                        app_state.update_status("Connected successfully!".to_string());

                        if !scopes.is_empty() {
                            for pane_idx in 0..app_state.num_panes {
                                let scope_idx = if pane_idx == 0 {
                                    0
                                } else {
                                    (app_state.panes[0].selected_scope_idx + 1) % scopes.len()
                                };
                                activate_scope(&scopes, scope_idx, &mut app_state.panes[pane_idx]);
                            }
                            send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx_clone);
                        }
                        let _ = cmd_tx_clone.send(WorkerCommand::GetSettings);
                    }
                    WorkerEvent::ConnectionFailed(err) => {
                        app_state.connection = ConnectionState::Disconnected(Some(err.clone()));
                        app_state.update_status(format!("Connection failed: {}", err));
                    }
                    WorkerEvent::NewData { scope_id, msg } => {
                        let mut routed = false;
                        for pane in &mut app_state.panes[..app_state.num_panes] {
                            for &(id, _) in &pane.worker_scopes {
                                if id == scope_id {
                                    if pane.in_group_mode {
                                        if let Some(snap) = pane.group_snapshots.get_mut(&scope_id) {
                                            snap.read_scope_msg(&msg, pane.stacking_enabled);
                                            routed = true;
                                        }
                                    } else if let Some(ref mut snapshot) = pane.active_snapshot {
                                        if snapshot.scope_id == scope_id as i32 {
                                            snapshot.read_scope_msg(&msg, pane.stacking_enabled);
                                            routed = true;
                                        }
                                    }
                                }
                            }
                        }
                        if routed {
                            app_state.frame_rate_counter += 1;
                        }
                    }
                    WorkerEvent::Error(err) => {
                        app_state.update_status(format!("Worker error: {}", err));
                    }
                    WorkerEvent::ScopesRefreshed { scopes } => {
                        if let ConnectionState::Connected {
                            scopes: ref mut ref_mut_scopes,
                            ..
                        } = app_state.connection
                        {
                            *ref_mut_scopes = scopes.clone();
                            app_state.update_status("Scopes refreshed successfully!".to_string());

                            if !scopes.is_empty() {
                                for pane_idx in 0..app_state.num_panes {
                                    let idx = app_state.panes[pane_idx]
                                        .selected_scope_idx
                                        .min(scopes.len() - 1);
                                    activate_scope(&scopes, idx, &mut app_state.panes[pane_idx]);
                                }
                                send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx_clone);
                            } else {
                                for pane in &mut app_state.panes {
                                    pane.active_snapshot = None;
                                    pane.group_snapshots.clear();
                                    pane.in_group_mode = false;
                                    pane.worker_scopes.clear();
                                }
                                send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx_clone);
                            }
                        } else {
                            app_state.update_status("Scopes refreshed, but not connected.".to_string());
                        }
                    }
                    WorkerEvent::SettingsRefreshed { settings } => {
                        app_state.settings = settings.clone();
                        for s in settings {
                            app_state.gui_settings.insert(s.name.clone(), s.value);
                        }
                    }
                    WorkerEvent::SettingUpdated { name, status } => {
                        if status == 0 {
                            app_state.update_status(format!("Setting '{}' updated!", name));
                        } else {
                            app_state.update_status(format!("Setting '{}' update failed ({})", name, status));
                        }
                        let _ = cmd_tx_clone.send(WorkerCommand::GetSettings);
                    }
                }
            }

            // ─── FPS Calculation ───
            let now = Instant::now();
            let elapsed = now.duration_since(app_state.last_frame_rate_calc);
            if elapsed >= Duration::from_secs(1) {
                app_state.current_fps = app_state.frame_rate_counter as f32 / elapsed.as_secs_f32();
                app_state.frame_rate_counter = 0;
                app_state.last_frame_rate_calc = now;
            }

            // ─── Initial Window Layout Constraints ───
            ui.set_window_pos_by_name_with_cond("Control Center", [10.0, 10.0], Condition::FirstUseEver);
            ui.set_window_size_by_name_with_cond("Control Center", [350.0, 680.0], Condition::FirstUseEver);

            ui.set_window_pos_by_name_with_cond("Scope Pane 1", [370.0, 10.0], Condition::FirstUseEver);
            ui.set_window_size_by_name_with_cond("Scope Pane 1", [880.0, 680.0], Condition::FirstUseEver);

            ui.set_window_pos_by_name_with_cond("Scope Pane 2", [370.0, 350.0], Condition::FirstUseEver);
            ui.set_window_size_by_name_with_cond("Scope Pane 2", [880.0, 340.0], Condition::FirstUseEver);

            // ─── 1. Control Center Window ───
            ui.window("Control Center")
                .build(|| {
                    ui.text("Announce URL Setup");
                    let mut ann_url = app_state.announce_url.clone();
                    if ui.input_text("##url", &mut ann_url).build() {
                        app_state.announce_url = ann_url;
                    }
                    ui.same_line();
                    if ui.button("Connect") {
                        app_state.update_status(format!("Connecting to {}...", app_state.announce_url));
                        let _ = cmd_tx_clone.send(WorkerCommand::Connect {
                            url: app_state.announce_url.clone(),
                        });
                    }

                    // Connection Status indicator
                    match &app_state.connection {
                        ConnectionState::Disconnected(err) => {
                            ui.text_colored([0.9, 0.2, 0.2, 1.0], "Disconnected");
                            if let Some(e) = err {
                                ui.text_wrapped(format!("Error: {}", e));
                            }
                        }
                        ConnectionState::Connecting => {
                            ui.text_colored([0.9, 0.9, 0.2, 1.0], "Connecting...");
                        }
                        ConnectionState::Connected { name, data_address, control_address, .. } => {
                            ui.text_colored([0.2, 0.9, 0.2, 1.0], format!("Connected: {}", name));
                            ui.text(format!("Data Addr: {}", data_address));
                            ui.text(format!("Control Addr: {}", control_address));
                        }
                    }

                    ui.separator();

                    ui.text("Collection Options");
                    if ui.checkbox("Auto Collect", &mut app_state.auto_collect_enabled) {
                        let _ = cmd_tx_clone.send(WorkerCommand::SetAutoCollect(app_state.auto_collect_enabled));
                    }

                    if !app_state.auto_collect_enabled {
                        ui.same_line();
                        if ui.button("Request Frame") {
                            let _ = cmd_tx_clone.send(WorkerCommand::RequestSingleFrame);
                        }
                    }

                    // Panes layout option
                    let mut current_panes_selection = app_state.num_panes - 1;
                    let pane_options = ["1 Pane", "2 Panes"];
                    if ui.combo("Layout", &mut current_panes_selection, &pane_options, |s| std::borrow::Cow::Borrowed(s)) {
                        let old_num = app_state.num_panes;
                        app_state.num_panes = current_panes_selection + 1;
                        if app_state.num_panes == 2 && old_num == 1 {
                            if let ConnectionState::Connected { scopes, .. } = &app_state.connection {
                                if !scopes.is_empty() {
                                    let new_idx = (app_state.panes[0].selected_scope_idx + 1) % scopes.len();
                                    activate_scope(scopes, new_idx, &mut app_state.panes[1]);
                                }
                            }
                        }
                        send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx_clone);
                    }

                    if ui.button("Refresh Scopes List") {
                        app_state.update_status("Refreshing scopes...".to_string());
                        let _ = cmd_tx_clone.send(WorkerCommand::RefreshScopes);
                    }
                    ui.same_line();
                    ui.text(format!("Data Rate: {:.1} FPS", app_state.current_fps));

                    ui.separator();

                    // Dynamic Producer Settings
                    if ui.collapsing_header("Producer Dynamic Settings", TreeNodeFlags::empty()) {
                        if app_state.settings.is_empty() {
                            ui.text("No producer settings available.");
                        } else {
                            for s in &app_state.settings {
                                let mut local_val = app_state.gui_settings.get(&s.name).cloned().unwrap_or(s.value.clone());
                                match &mut local_val {
                                    SettingValue::Bool(val) => {
                                        if ui.checkbox(&s.name, val) {
                                            let _ = cmd_tx_clone.send(WorkerCommand::UpdateSetting {
                                                name: s.name.clone(),
                                                setting_type: s.setting_type,
                                                value: SettingValue::Bool(*val),
                                            });
                                            app_state.gui_settings.insert(s.name.clone(), SettingValue::Bool(*val));
                                        }
                                    }
                                    SettingValue::Int32(val) => {
                                        if ui.input_int(&s.name, val) {
                                            let _ = cmd_tx_clone.send(WorkerCommand::UpdateSetting {
                                                name: s.name.clone(),
                                                setting_type: s.setting_type,
                                                value: SettingValue::Int32(*val),
                                            });
                                            app_state.gui_settings.insert(s.name.clone(), SettingValue::Int32(*val));
                                        }
                                    }
                                    SettingValue::Float(val) => {
                                        if ui.input_float(&s.name, val) {
                                            let _ = cmd_tx_clone.send(WorkerCommand::UpdateSetting {
                                                name: s.name.clone(),
                                                setting_type: s.setting_type,
                                                value: SettingValue::Float(*val),
                                            });
                                            app_state.gui_settings.insert(s.name.clone(), SettingValue::Float(*val));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    ui.separator();

                    // Status Log History child window
                    ui.text("Status Message Log");
                    ui.child_window("status_logs")
                        .size([0.0, 150.0])
                        .border(true)
                        .build(ui, || {
                            for log in &app_state.status_log {
                                ui.text(log);
                            }
                        });
                });

            // ─── 2. Scope Panes ───
            if let ConnectionState::Connected { scopes, .. } = &app_state.connection {
                let scope_names: Vec<String> = scopes.iter().map(|s| {
                    let typ_str = match s.scope_type {
                        ScopeType::IqData => "IQ",
                        ScopeType::Real => "Real",
                        ScopeType::Int32 => "Int32",
                        ScopeType::Float => "Float",
                    };
                    if s.group.is_empty() {
                        format!("{} [{}]", s.name, typ_str)
                    } else {
                        format!("{} [{}] (Group: {})", s.name, typ_str, s.group)
                    }
                }).collect();

                let mut merged_scopes_dirty = false;

                for pane_idx in 0..app_state.num_panes {
                    let title = format!("Scope Pane {}", pane_idx + 1);
                    ui.window(title)
                        .build(|| {
                            let pane = &mut app_state.panes[pane_idx];
                            if scopes.is_empty() {
                                ui.text("No scopes available.");
                                return;
                            }
                            pane.selected_scope_idx = pane.selected_scope_idx.min(scopes.len() - 1);

                            // Scope Selection Combo
                            let mut current_idx = pane.selected_scope_idx;
                            if ui.combo("Select Scope", &mut current_idx, &scope_names, |s| std::borrow::Cow::Borrowed(s)) {
                                activate_scope(scopes, current_idx, pane);
                                merged_scopes_dirty = true;
                            }

                            // Group Ungroup Toggle
                            let active_scope = &scopes[pane.selected_scope_idx];
                            let has_group = !active_scope.group.is_empty();
                            if has_group {
                                ui.same_line();
                                if ui.checkbox("Ungrouped View", &mut pane.ungrouped) {
                                    activate_scope(scopes, pane.selected_scope_idx, pane);
                                    merged_scopes_dirty = true;
                                }
                            }

                            // Filters and Stacking Collapsible Options
                            if ui.collapsing_header("Signal Filters & Stacking", TreeNodeFlags::empty()) {
                                let mut filter_changed = false;
                                filter_changed |= ui.checkbox("Filter Enabled", &mut pane.filter_enabled);
                                ui.same_line();
                                filter_changed |= ui.input_float("Cutoff", &mut pane.filter_cutoff);
                                ui.same_line();
                                filter_changed |= ui.input_float("Percentage", &mut pane.filter_percentage);

                                if filter_changed {
                                    let _ = cmd_tx_clone.send(WorkerCommand::SetFilter {
                                        enabled: pane.filter_enabled,
                                        cutoff: pane.filter_cutoff,
                                        percentage: pane.filter_percentage,
                                    });
                                }

                                ui.separator();

                                let mut stack_changed = false;
                                stack_changed |= ui.checkbox("Stacking Enabled", &mut pane.stacking_enabled);
                                ui.same_line();
                                let mut s_sz = pane.stacking_size as i32;
                                if ui.input_int("Stacking Size", &mut s_sz) {
                                    pane.stacking_size = (s_sz.max(1000) as usize).min(100000);
                                    stack_changed = true;
                                }
                                if stack_changed {
                                    if let Some(ref mut s) = pane.active_snapshot {
                                        s.max_stacked_size = pane.stacking_size;
                                    }
                                    for s in pane.group_snapshots.values_mut() {
                                        s.max_stacked_size = pane.stacking_size;
                                    }
                                }
                            }

                            // Determine tabs to render
                            let show_scatter_rms_hist2d = !pane.in_group_mode && (active_scope.scope_type == ScopeType::IqData);

                            // Get plot UI if ImPlot is active
                            if let Some(implot_ctx) = addons.implot {
                                let plot_ui = ui.implot(implot_ctx);
                                if let Some(_tab_bar) = ui.tab_bar(format!("ScopeTabs_{}", pane_idx)) {
                                    // ── Scatter Tab ──
                                    if show_scatter_rms_hist2d {
                                        if let Some(_tab) = ui.tab_item("Scatter (IQ)") {
                                            if let Some(snapshot) = &pane.active_snapshot {
                                                if !snapshot.real.is_empty() {
                                                    let label = format!("Scatter Plot (Scope {})", snapshot.scope_id);
                                                    if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                        let lim = if snapshot.max_iq > 0.0 { snapshot.max_iq * 1.1 } else { 1.0 };
                                                        plot_ui.set_next_axes_limits(-lim, lim, -lim, lim, PlotCond::Always);
                                                        let _ = plot_ui.scatter_plot("IQ Constellation", &snapshot.real, &snapshot.imag);
                                                        token.end();
                                                    }
                                                } else {
                                                    ui.text("No signal data received yet.");
                                                }
                                            }
                                        }
                                    }

                                    // ── RMS Power Tab ──
                                    if show_scatter_rms_hist2d {
                                        if let Some(_tab) = ui.tab_item("RMS Power") {
                                            if let Some(snapshot) = &pane.active_snapshot {
                                                if !snapshot.power.is_empty() {
                                                    let label = format!("RMS Power (Scope {})", snapshot.scope_id);
                                                    if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                        let power_f64: Vec<f64> = snapshot.power.iter().map(|&x| x as f64).collect();
                                                        let _ = plot_ui.simple_line_plot("Power", &power_f64);
                                                        token.end();
                                                    }
                                                } else {
                                                    ui.text("No signal data received yet.");
                                                }
                                            }
                                        }
                                    }

                                    // ── Waveform Tab ──
                                    if let Some(_tab) = ui.tab_item("Waveform") {
                                        if pane.in_group_mode {
                                            if !pane.group_snapshots.is_empty() {
                                                let label = format!("Group Waveform (Group {})", active_scope.group);
                                                if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                    for (&member_id, snap) in &pane.group_snapshots {
                                                        if !snap.real.is_empty() {
                                                            let _ = plot_ui.simple_line_plot(&format!("Scope {} (Real)", member_id), &snap.real);
                                                        }
                                                    }
                                                    token.end();
                                                }
                                            } else {
                                                ui.text("No group member data received yet.");
                                            }
                                        } else if let Some(snapshot) = &pane.active_snapshot {
                                            if !snapshot.real.is_empty() {
                                                let label = format!("Waveform (Scope {})", snapshot.scope_id);
                                                if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                    let _ = plot_ui.simple_line_plot("Real", &snapshot.real);
                                                    if !snapshot.imag.is_empty() {
                                                        let _ = plot_ui.simple_line_plot("Imag", &snapshot.imag);
                                                    }
                                                    token.end();
                                                }
                                            } else {
                                                ui.text("No signal data received yet.");
                                            }
                                        }
                                    }

                                    // ── Histogram Tab ──
                                    if let Some(_tab) = ui.tab_item("Histogram") {
                                        if pane.in_group_mode {
                                            if !pane.group_snapshots.is_empty() {
                                                let label = format!("Group Amplitude Distribution (Group {})", active_scope.group);
                                                if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                    for (&member_id, snap) in &pane.group_snapshots {
                                                        if !snap.real.is_empty() {
                                                            let _ = plot_ui.histogram_plot(&format!("Scope {} Dist", member_id), &snap.real);
                                                        }
                                                    }
                                                    token.end();
                                                }
                                            } else {
                                                ui.text("No group member data received yet.");
                                            }
                                        } else if let Some(snapshot) = &pane.active_snapshot {
                                            if !snapshot.real.is_empty() {
                                                let label = format!("Amplitude Distribution (Scope {})", snapshot.scope_id);
                                                if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                    let _ = plot_ui.histogram_plot("Real", &snapshot.real);
                                                    if !snapshot.imag.is_empty() {
                                                        let _ = plot_ui.histogram_plot("Imag", &snapshot.imag);
                                                    }
                                                    token.end();
                                                }
                                            } else {
                                                ui.text("No signal data received yet.");
                                            }
                                        }
                                    }

                                    // ── 2D Density Tab ──
                                    if show_scatter_rms_hist2d {
                                        if let Some(_tab) = ui.tab_item("2D Density") {
                                            if let Some(snapshot) = &pane.active_snapshot {
                                                if !snapshot.real.is_empty() && !snapshot.imag.is_empty() {
                                                    let label = format!("2D IQ Density (Scope {})", snapshot.scope_id);
                                                    if let Some(token) = plot_ui.begin_plot_with_size(&label, [-1.0, -1.0]) {
                                                        let _ = plot_ui.histogram_2d_plot("Density", &snapshot.real, &snapshot.imag);
                                                        token.end();
                                                    }
                                                } else {
                                                    ui.text("No IQ signal data received yet.");
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        });
                }

                if merged_scopes_dirty {
                    send_merged_scopes(&app_state.panes, app_state.num_panes, &cmd_tx_clone);
                }
            } else {
                // If not connected, show a nice landing/status message in the main area
                ui.window("Scope View")
                    .build(|| {
                        ui.text_wrapped("Not connected to announcer. Please check settings and click 'Connect' in the Control Center panel.");
                    });
            }
        })
        .run()?;

    Ok(())
}
