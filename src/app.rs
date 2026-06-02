/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::mpsc::Sender;

use crate::consumer::{IQSnapshot, ScopeConfig, ScopeType, WorkerCommand};

// ── Connection state ───────────────────────────────────────────────────────────

pub enum ConnectionState {
    Disconnected(Option<String>),
    Connecting,
    Connected {
        name: String,
        data_address: String,
        control_address: String,
        scopes: Vec<ScopeConfig>,
    },
}

// ── Plot pane (shared between TUI and GUI) ─────────────────────────────────────

pub struct PlotPane {
    pub selected_scope_idx: usize,
    pub stacking_enabled: bool,
    pub stacking_size: usize,
    pub filter_enabled: bool,
    pub filter_cutoff: f32,
    pub filter_percentage: f32,
    pub active_snapshot: Option<IQSnapshot>,
    pub group_snapshots: HashMap<usize, IQSnapshot>,
    pub in_group_mode: bool,
    pub ungrouped: bool,
    pub worker_scopes: Vec<(usize, ScopeType)>,
}

impl PlotPane {
    pub fn new() -> Self {
        Self {
            selected_scope_idx: 0,
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

impl Default for PlotPane {
    fn default() -> Self {
        Self::new()
    }
}

// ── Shared logic ───────────────────────────────────────────────────────────────

/// Activate scope at `idx` for a pane — handles group vs. solo mode.
/// Has bounds guard: safe to call with any `idx` value.
pub fn activate_scope(scopes: &[ScopeConfig], idx: usize, pane: &mut PlotPane) {
    if scopes.is_empty() {
        return;
    }
    let idx = idx.min(scopes.len() - 1);
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
}

/// Trait for types that expose a worker scope list — lets `send_merged_scopes`
/// work generically over both the GUI's `PlotPane` and the TUI's wrapper.
pub trait HasWorkerScopes {
    fn worker_scopes(&self) -> &[(usize, ScopeType)];
}

impl HasWorkerScopes for PlotPane {
    fn worker_scopes(&self) -> &[(usize, ScopeType)] {
        &self.worker_scopes
    }
}

/// Deduplicate all pane scope lists and tell the worker to fetch them.
pub fn send_merged_scopes<P: HasWorkerScopes>(
    panes: &[P],
    num_panes: usize,
    cmd_tx: &Sender<WorkerCommand>,
) {
    let mut all: Vec<(usize, ScopeType)> = Vec::new();
    let mut seen = HashSet::new();
    for pane in &panes[..num_panes] {
        for &(id, stype) in pane.worker_scopes() {
            if seen.insert(id) {
                all.push((id, stype));
            }
        }
    }
    let _ = cmd_tx.send(WorkerCommand::SelectGroup { members: all });
}
