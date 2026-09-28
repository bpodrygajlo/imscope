/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! Ratatui rendering for a single scope's plot, shared between the
//! interactive TUI (`imscope-tui`) and the low-interactivity single-scope
//! viewer (`imscope-view`).

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Bar, BarChart, BarGroup, Block, Borders, Paragraph,
        canvas::{Canvas, Points},
    },
};

use crate::consumer::{self, IQSnapshot, ScopeDomain, ScopeType};
use crate::dsp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlotKind {
    Scatter,
    Rms,
    Waveform,
    Histogram,
    Spectrum,
}

impl PlotKind {
    pub const ALL: [PlotKind; 5] = [
        PlotKind::Scatter,
        PlotKind::Rms,
        PlotKind::Waveform,
        PlotKind::Histogram,
        PlotKind::Spectrum,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            PlotKind::Scatter => "Scatter",
            PlotKind::Rms => "RMS Power",
            PlotKind::Waveform => "Waveform",
            PlotKind::Histogram => "Histogram",
            PlotKind::Spectrum => "Spectrum",
        }
    }
}

/// Renders the plot for a single (non-grouped) scope's snapshot into `area`,
/// using `plot_block` as the surrounding bordered block. `waiting_hint` is
/// shown (on its own line) under "No scope data received yet." when the
/// snapshot is empty.
#[allow(clippy::too_many_arguments)]
pub fn render_single_scope(
    frame: &mut Frame,
    area: Rect,
    plot_block: Block<'static>,
    kind: PlotKind,
    snapshot: &IQSnapshot,
    scope_type: Option<ScopeType>,
    scope_domain: Option<ScopeDomain>,
    waiting_hint: &str,
) {
    if snapshot.size() == 0 {
        let msg = Paragraph::new(format!("\n\nNo scope data received yet.\n{}", waiting_hint))
            .alignment(Alignment::Center)
            .fg(Color::DarkGray)
            .block(plot_block);
        frame.render_widget(msg, area);
        return;
    }

    let is_scalar = matches!(scope_type, Some(ScopeType::Int32) | Some(ScopeType::Float));

    match kind {
        PlotKind::Scatter => {
            if scope_type != Some(ScopeType::IqData) {
                let msg = Paragraph::new(
                    "\n\nScatter/Constellation plot is only available for IQ scopes.",
                )
                .alignment(Alignment::Center)
                .fg(Color::Yellow)
                .block(plot_block);
                frame.render_widget(msg, area);
                return;
            }
            let limit = (snapshot.max_iq * 1.2).max(1.0);
            let skip = (snapshot.real.len() / 2000).max(1);
            let points = snapshot
                .real
                .iter()
                .zip(snapshot.imag.iter())
                .step_by(skip)
                .map(|(&r, &im)| (r, im))
                .collect::<Vec<(f64, f64)>>();
            let canvas = Canvas::default()
                .block(plot_block.title(" Constellation Scatterplot (Imag vs Real) "))
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
            frame.render_widget(canvas, area);
        }
        PlotKind::Rms => {
            if scope_type != Some(ScopeType::IqData) {
                let msg = Paragraph::new("\n\nRMS Power plot is only available for IQ scopes.")
                    .alignment(Alignment::Center)
                    .fg(Color::Yellow)
                    .block(plot_block);
                frame.render_widget(msg, area);
                return;
            }
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
            frame.render_widget(canvas, area);
        }
        PlotKind::Waveform => {
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
            frame.render_widget(canvas, area);
        }
        PlotKind::Histogram => {
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
                let idx = (((val as f32 - range_lo) / bin_width) as usize).min(num_bins - 1);
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
                .block(plot_block.title(" Amplitude Density Distribution (1D Real Histogram) "))
                .data(BarGroup::default().bars(&bars))
                .bar_width(3)
                .bar_gap(1)
                .value_style(
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .label_style(Style::default().fg(Color::Gray));
            frame.render_widget(chart, area);
        }
        PlotKind::Spectrum => {
            let domain = scope_domain.unwrap_or(consumer::ScopeDomain::Time);
            let spectrum = dsp::compute_spectrum_db(&snapshot.real, &snapshot.imag, domain);
            if spectrum.is_empty() {
                let msg = Paragraph::new("\n\nNo signal data received yet.")
                    .alignment(Alignment::Center)
                    .fg(Color::DarkGray)
                    .block(plot_block);
                frame.render_widget(msg, area);
                return;
            }
            let y_lo = spectrum.iter().cloned().fold(f64::MAX, f64::min);
            let y_hi = spectrum
                .iter()
                .cloned()
                .fold(f64::MIN, f64::max)
                .max(y_lo + 1.0);
            let num_bins = spectrum.len();
            let domain_label = match domain {
                consumer::ScopeDomain::Time => "FFT, Hann window",
                consumer::ScopeDomain::Frequency => "direct, already freq-domain",
            };
            let canvas = Canvas::default()
                .block(plot_block.title(format!(
                    " Spectrum [{}] (dB vs. frequency bin) ",
                    domain_label
                )))
                .x_bounds([0.0, num_bins as f64])
                .y_bounds([y_lo, y_hi])
                .paint(move |ctx| {
                    let step = (num_bins / 500).max(1);
                    for i in (0..num_bins.saturating_sub(step)).step_by(step) {
                        ctx.draw(&ratatui::widgets::canvas::Line {
                            x1: i as f64,
                            y1: spectrum[i],
                            x2: (i + step) as f64,
                            y2: spectrum[i + step],
                            color: Color::Cyan,
                        });
                    }
                });
            frame.render_widget(canvas, area);
        }
    }
}

/// Renders the "Active Frame Metadata" footer block for a single scope.
pub fn render_metadata_footer(
    frame: &mut Frame,
    area: Rect,
    snapshot: Option<&IQSnapshot>,
    scope_type: Option<ScopeType>,
) {
    let meta_block = Block::default()
        .borders(Borders::ALL)
        .title(" Active Frame Metadata ")
        .border_style(Style::default().fg(Color::DarkGray));

    let Some(snapshot) = snapshot else {
        frame.render_widget(
            Paragraph::new("No active scope metadata.")
                .fg(Color::DarkGray)
                .block(meta_block),
            area,
        );
        return;
    };

    let is_scalar = matches!(scope_type, Some(ScopeType::Int32) | Some(ScopeType::Float));
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
            Span::raw("   RMS Power: "),
            Span::styled(
                format!("{:.2}", consumer::rms_power(&snapshot.power)),
                Style::default().fg(Color::Green),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(meta_lines).block(meta_block), area);
}
