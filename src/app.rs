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
    /// Set by an "Autoscale" button press; consumed (and reset) by whichever
    /// tab is currently visible, applying a one-shot axes-fit there.
    pub autoscale_requested: bool,
    /// Persistence ("color-graded"/phosphor-trail) display for the Scatter
    /// tab: a decaying PERSISTENCE_ROWS x PERSISTENCE_COLS grid, row-major.
    pub persistence_enabled: bool,
    pub persistence_decay: f32,
    pub persistence_grid: Vec<f32>,
    /// Measurement cursors (shared across the Waveform/Power/Spectrum tabs,
    /// since only one tab is visible at a time). Positions are in sample
    /// index / bin units, not time — the protocol has no sample-rate.
    pub cursors_enabled: bool,
    pub cursor_x1: f64,
    pub cursor_x2: f64,
}

pub const PERSISTENCE_ROWS: usize = 96;
pub const PERSISTENCE_COLS: usize = 96;

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
            autoscale_requested: false,
            persistence_enabled: false,
            persistence_decay: 0.97,
            persistence_grid: vec![0.0; PERSISTENCE_ROWS * PERSISTENCE_COLS],
            cursors_enabled: false,
            cursor_x1: 0.0,
            cursor_x2: 0.0,
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

/// Decay every cell of a persistence grid, then bin each `(real[i],
/// imag[i])` point into it — the accumulation step behind the Scatter tab's
/// color-graded/phosphor-trail display. `grid` is `rows*cols`, row-major,
/// covering the square `[-lim, lim] x [-lim, lim]` (real -> column, imag ->
/// row, flipped so positive imaginary values land toward row 0). Points
/// outside that square, or a non-positive `lim`, leave the grid decayed but
/// otherwise untouched.
pub fn accumulate_persistence(
    grid: &mut [f32],
    rows: usize,
    cols: usize,
    decay: f32,
    real: &[f64],
    imag: &[f64],
    lim: f64,
) {
    for cell in grid.iter_mut() {
        *cell *= decay;
    }
    if lim <= 0.0 || rows == 0 || cols == 0 {
        return;
    }

    let n = real.len().min(imag.len());
    for i in 0..n {
        let r = real[i];
        let im = imag[i];
        if r < -lim || r > lim || im < -lim || im > lim {
            continue;
        }
        let col = (((r + lim) / (2.0 * lim)) * cols as f64) as usize;
        let row = (((-im + lim) / (2.0 * lim)) * rows as f64) as usize;
        let col = col.min(cols - 1);
        let row = row.min(rows - 1);
        if let Some(cell) = grid.get_mut(row * cols + col) {
            *cell += 1.0;
        }
    }
}

/// Default cursor positions (25%/75% of the current sample count) used when
/// cursors are first enabled for a pane.
pub fn default_cursor_positions(sample_count: usize) -> (f64, f64) {
    let n = sample_count as f64;
    (0.25 * n, 0.75 * n)
}

/// Looks up the value in `data` nearest to sample index `x`, clamping to
/// the valid range. Returns `None` for empty data.
pub fn nearest_sample(data: &[f64], x: f64) -> Option<f64> {
    if data.is_empty() {
        return None;
    }
    let idx = x.round().clamp(0.0, (data.len() - 1) as f64) as usize;
    data.get(idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consumer::ScopeDomain;
    use std::sync::mpsc;

    fn scope(name: &str, group: &str, scope_type: ScopeType) -> ScopeConfig {
        ScopeConfig {
            name: name.to_string(),
            group: group.to_string(),
            scope_type,
            domain: ScopeDomain::Time,
        }
    }

    #[test]
    fn activate_scope_empty_scopes_is_noop() {
        let mut pane = PlotPane::new();
        activate_scope(&[], 3, &mut pane);
        assert_eq!(pane.selected_scope_idx, 0);
        assert!(pane.worker_scopes.is_empty());
    }

    #[test]
    fn activate_scope_clamps_out_of_bounds_idx() {
        let scopes = vec![scope("a", "", ScopeType::Real)];
        let mut pane = PlotPane::new();
        activate_scope(&scopes, 5, &mut pane);
        assert_eq!(pane.selected_scope_idx, 0);
    }

    #[test]
    fn activate_scope_ungrouped_scope_uses_solo_mode() {
        let scopes = vec![
            scope("a", "", ScopeType::Real),
            scope("b", "", ScopeType::Real),
        ];
        let mut pane = PlotPane::new();
        activate_scope(&scopes, 1, &mut pane);

        assert!(!pane.in_group_mode);
        assert!(pane.active_snapshot.is_some());
        assert!(pane.group_snapshots.is_empty());
        assert_eq!(pane.worker_scopes, vec![(1, ScopeType::Real)]);
    }

    #[test]
    fn activate_scope_grouped_scope_collects_all_members() {
        let scopes = vec![
            scope("a", "g1", ScopeType::Real),
            scope("b", "other", ScopeType::Real),
            scope("c", "g1", ScopeType::IqData),
        ];
        let mut pane = PlotPane::new();
        activate_scope(&scopes, 0, &mut pane);

        assert!(pane.in_group_mode);
        assert!(pane.active_snapshot.is_none());
        assert_eq!(pane.group_snapshots.len(), 2);
        assert!(pane.group_snapshots.contains_key(&0));
        assert!(pane.group_snapshots.contains_key(&2));
        let mut worker_scopes = pane.worker_scopes.clone();
        worker_scopes.sort_by_key(|(id, _)| *id);
        assert_eq!(
            worker_scopes,
            vec![(0, ScopeType::Real), (2, ScopeType::IqData)]
        );
    }

    #[test]
    fn activate_scope_ungrouped_flag_forces_solo_mode_even_in_group() {
        let scopes = vec![
            scope("a", "g1", ScopeType::Real),
            scope("b", "g1", ScopeType::Real),
        ];
        let mut pane = PlotPane::new();
        pane.ungrouped = true;
        activate_scope(&scopes, 0, &mut pane);

        assert!(!pane.in_group_mode);
        assert_eq!(pane.worker_scopes, vec![(0, ScopeType::Real)]);
    }

    #[test]
    fn activate_scope_applies_stacking_size_to_new_snapshots() {
        let scopes = vec![scope("a", "", ScopeType::Real)];
        let mut pane = PlotPane::new();
        pane.stacking_size = 42;
        activate_scope(&scopes, 0, &mut pane);

        assert_eq!(pane.active_snapshot.unwrap().max_stacked_size, 42);
    }

    struct FakePane(Vec<(usize, ScopeType)>);
    impl HasWorkerScopes for FakePane {
        fn worker_scopes(&self) -> &[(usize, ScopeType)] {
            &self.0
        }
    }

    #[test]
    fn send_merged_scopes_dedupes_across_panes() {
        let panes = vec![
            FakePane(vec![(0, ScopeType::Real), (1, ScopeType::Real)]),
            FakePane(vec![(1, ScopeType::Real), (2, ScopeType::IqData)]),
        ];
        let (tx, rx) = mpsc::channel();
        send_merged_scopes(&panes, panes.len(), &tx);

        match rx.try_recv().unwrap() {
            WorkerCommand::SelectGroup { members } => {
                assert_eq!(
                    members,
                    vec![
                        (0, ScopeType::Real),
                        (1, ScopeType::Real),
                        (2, ScopeType::IqData)
                    ]
                );
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn send_merged_scopes_respects_num_panes_limit() {
        let panes = vec![
            FakePane(vec![(0, ScopeType::Real)]),
            FakePane(vec![(1, ScopeType::Real)]),
        ];
        let (tx, rx) = mpsc::channel();
        // Only consider the first pane even though two are provided.
        send_merged_scopes(&panes, 1, &tx);

        match rx.try_recv().unwrap() {
            WorkerCommand::SelectGroup { members } => {
                assert_eq!(members, vec![(0, ScopeType::Real)]);
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn accumulate_persistence_decays_existing_cells() {
        let mut grid = vec![1.0f32; 4];
        accumulate_persistence(&mut grid, 2, 2, 0.5, &[], &[], 1.0);
        assert_eq!(grid, vec![0.5, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn accumulate_persistence_bins_corner_points() {
        let mut grid = vec![0.0f32; 16]; // 4x4
        // Top-right (max real, max imag) and bottom-left (min real, min imag).
        accumulate_persistence(&mut grid, 4, 4, 1.0, &[2.0, -2.0], &[2.0, -2.0], 2.0);
        assert_eq!(grid[3], 1.0); // row 0 (top), col 3 (right)
        assert_eq!(grid[3 * 4 + 0], 1.0); // row 3 (bottom), col 0 (left)
        assert_eq!(grid.iter().sum::<f32>(), 2.0);
    }

    #[test]
    fn accumulate_persistence_drops_out_of_range_points() {
        let mut grid = vec![0.0f32; 4];
        accumulate_persistence(&mut grid, 2, 2, 1.0, &[100.0], &[0.0], 1.0);
        assert_eq!(grid, vec![0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn accumulate_persistence_ignores_non_positive_lim() {
        let mut grid = vec![1.0f32; 4];
        accumulate_persistence(&mut grid, 2, 2, 1.0, &[0.0], &[0.0], 0.0);
        // Decay (by 1.0, a no-op here) still applies, but no binning happens.
        assert_eq!(grid, vec![1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn default_cursor_positions_is_quarter_and_three_quarters() {
        assert_eq!(default_cursor_positions(100), (25.0, 75.0));
        assert_eq!(default_cursor_positions(0), (0.0, 0.0));
    }

    #[test]
    fn nearest_sample_empty_is_none() {
        assert_eq!(nearest_sample(&[], 5.0), None);
    }

    #[test]
    fn nearest_sample_rounds_to_closest_index() {
        let data = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(nearest_sample(&data, 1.4), Some(20.0));
        assert_eq!(nearest_sample(&data, 1.6), Some(30.0));
    }

    #[test]
    fn nearest_sample_clamps_out_of_range_x() {
        let data = [10.0, 20.0, 30.0];
        assert_eq!(nearest_sample(&data, -5.0), Some(10.0));
        assert_eq!(nearest_sample(&data, 100.0), Some(30.0));
    }
}
