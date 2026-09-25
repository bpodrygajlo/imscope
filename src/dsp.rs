/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::consumer::ScopeDomain;

const EPSILON: f64 = 1e-12;

fn to_db(magnitude: f64) -> f64 {
    20.0 * magnitude.max(EPSILON).log10()
}

fn hann_window(n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![1.0; n];
    }
    (0..n)
        .map(|i| {
            0.5 * (1.0
                - (2.0 * std::f64::consts::PI * i as f64 / (n as f64 - 1.0)).cos())
        })
        .collect()
}

/// Computes a magnitude spectrum in dB from a snapshot's `real`/`imag`
/// samples.
///
/// - `ScopeDomain::Time`: applies a Hann window and runs an FFT. Complex
///   (IQ) input produces a centered, two-sided spectrum (DC at the middle);
///   real-only input produces a one-sided spectrum (`0..=N/2`, DC first).
/// - `ScopeDomain::Frequency`: the samples are already spectral bins (e.g.
///   post-FFT resource-grid symbols from a producer's own pipeline) - no
///   windowing or FFT is applied, each bin's magnitude is converted to dB
///   directly, in its original order.
pub fn compute_spectrum_db(real: &[f64], imag: &[f64], domain: ScopeDomain) -> Vec<f64> {
    let is_iq = !imag.is_empty();
    let n = if is_iq {
        real.len().min(imag.len())
    } else {
        real.len()
    };
    if n == 0 {
        return Vec::new();
    }

    match domain {
        ScopeDomain::Frequency => (0..n)
            .map(|i| {
                let im = if is_iq { imag[i] } else { 0.0 };
                to_db((real[i] * real[i] + im * im).sqrt())
            })
            .collect(),
        ScopeDomain::Time => {
            let window = hann_window(n);
            let mut buffer: Vec<Complex<f64>> = (0..n)
                .map(|i| {
                    let im = if is_iq { imag[i] } else { 0.0 };
                    Complex::new(real[i] * window[i], im * window[i])
                })
                .collect();

            let mut planner = FftPlanner::<f64>::new();
            let fft = planner.plan_fft_forward(n);
            fft.process(&mut buffer);

            if is_iq {
                // Two-sided, centered spectrum: bin n/2 (rounded down) is DC.
                let mut shifted = vec![Complex::new(0.0, 0.0); n];
                let half = n / 2;
                for (k, shifted_val) in shifted.iter_mut().enumerate() {
                    *shifted_val = buffer[(k + half) % n];
                }
                shifted.iter().map(|c| to_db(c.norm())).collect()
            } else {
                // One-sided spectrum: DC (bin 0) through Nyquist.
                let last = n / 2;
                buffer[..=last].iter().map(|c| to_db(c.norm())).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_spectrum_db_empty_input_is_empty() {
        assert!(compute_spectrum_db(&[], &[], ScopeDomain::Time).is_empty());
    }

    #[test]
    fn compute_spectrum_db_time_domain_real_tone_peaks_at_bin() {
        // A pure cosine at an exact integer bin frequency (5 cycles over 64
        // samples) should peak in the one-sided spectrum at bin 5.
        let n = 64;
        let bin = 5;
        let real: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * bin as f64 * i as f64 / n as f64).cos())
            .collect();

        let spectrum = compute_spectrum_db(&real, &[], ScopeDomain::Time);
        assert_eq!(spectrum.len(), n / 2 + 1);

        let (peak_idx, _) = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert_eq!(peak_idx, bin);
    }

    #[test]
    fn compute_spectrum_db_time_domain_iq_is_centered_two_sided() {
        let n = 32;
        let real = vec![1.0; n];
        let imag = vec![0.0; n];
        let spectrum = compute_spectrum_db(&real, &imag, ScopeDomain::Time);
        // Full two-sided spectrum: one bin per sample.
        assert_eq!(spectrum.len(), n);
        // A DC-only signal should peak exactly at the centered DC bin.
        let (peak_idx, _) = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert_eq!(peak_idx, n / 2);
    }

    #[test]
    fn compute_spectrum_db_frequency_domain_skips_fft() {
        // Already-frequency-domain data: magnitude is computed directly,
        // bin order is preserved (no FFT, no windowing).
        let real = vec![3.0, 0.0];
        let imag = vec![4.0, 0.0];
        let spectrum = compute_spectrum_db(&real, &imag, ScopeDomain::Frequency);
        assert_eq!(spectrum.len(), 2);
        assert!((spectrum[0] - 20.0 * 5.0f64.log10()).abs() < 1e-9);
        assert!(spectrum[1] < spectrum[0]);
    }
}
