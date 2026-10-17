//! WSOLA time stretch, ported from Chromium's `media/filters/audio_renderer_algorithm.cc`
//! and `wsola_internals.cc` (Copyright The Chromium Authors, BSD-3-Clause; full notice in
//! `LICENSE-chromium`), the pitch-preserving playback rate YouTube gets in Chrome.

const OLA_WINDOW_MS: f64 = 20.0;
const SEARCH_INTERVAL_MS: f64 = 30.0;
const EXCLUDE_INTERVAL_FRAMES: i64 = 160;
const SEARCH_DECIMATION: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Passthrough,
    Wsola,
}

/// Planar audio, one `Vec` per channel.
type Bus = Vec<Vec<f32>>;

pub struct Wsola {
    channels: usize,
    input: Bus,
    ola_window_size: usize,
    ola_hop_size: usize,
    num_candidate_blocks: usize,
    search_block_center_offset: i64,
    ola_window: Vec<f32>,
    transition_window: Vec<f32>,
    wsola_output: Bus,
    optimal_block: Bus,
    search_block: Bus,
    target_block: Bus,
    output_time: f64,
    search_block_index: i64,
    target_block_index: i64,
    num_complete_frames: usize,
    mode: Option<Mode>,
    /// The rate the current WSOLA run was started with.
    run_rate: f64,
    /// Input frames dropped from the front of `input` since the last flush.
    input_base: u64,
    /// Input frames the emitted output stands for: passthrough frames plus WSOLA output
    /// frames times their rate. A run resumes from here, so a rate change neither replays
    /// nor skips audio.
    consumed: f64,
}

fn time_to_frames(ms: f64, rate: u32) -> usize {
    (ms / 1000.0 * f64::from(rate)).round() as usize
}

fn bus(channels: usize, frames: usize) -> Bus {
    vec![vec![0.0; frames]; channels]
}

fn periodic_hanning(size: usize) -> Vec<f32> {
    let scale = 2.0 * std::f32::consts::PI / size as f32;
    (0..size).map(|n| 0.5 * (1.0 - (n as f32 * scale).cos())).collect()
}

impl Wsola {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        let num_candidate_blocks = time_to_frames(SEARCH_INTERVAL_MS, sample_rate);
        let mut ola_window_size = time_to_frames(OLA_WINDOW_MS, sample_rate);
        ola_window_size += ola_window_size & 1;
        let ola_hop_size = ola_window_size / 2;
        Self {
            channels,
            input: bus(channels, 0),
            ola_window_size,
            ola_hop_size,
            num_candidate_blocks,
            search_block_center_offset: (num_candidate_blocks / 2 + (ola_window_size / 2 - 1)) as i64,
            ola_window: periodic_hanning(ola_window_size),
            transition_window: periodic_hanning(ola_window_size * 2),
            wsola_output: bus(channels, ola_window_size + ola_hop_size),
            optimal_block: bus(channels, ola_window_size),
            search_block: bus(channels, num_candidate_blocks + ola_window_size - 1),
            target_block: bus(channels, ola_window_size),
            output_time: 0.0,
            search_block_index: 0,
            target_block_index: 0,
            num_complete_frames: 0,
            mode: None,
            run_rate: 1.0,
            input_base: 0,
            consumed: 0.0,
        }
    }

    /// Frames queued and not yet consumed.
    pub fn queued_frames(&self) -> usize {
        self.input[0].len()
    }

    pub fn push_interleaved(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            for (ch, s) in frame.iter().enumerate() {
                self.input[ch].push(*s);
            }
        }
    }

    /// Drops all queued input and pending output (a flush).
    pub fn flush(&mut self) {
        for ch in &mut self.input {
            ch.clear();
        }
        self.reset_wsola();
        self.input_base = 0;
        self.consumed = 0.0;
    }

    /// Input frames, since the last flush, that the emitted output stands for.
    pub fn consumed(&self) -> f64 {
        self.consumed
    }

    /// Drops queued input the emitted output already stands for.
    fn align_to_consumed(&mut self) {
        let behind = self.consumed.round() as i64 - self.input_base as i64;
        let n = behind.clamp(0, self.queued_frames() as i64) as usize;
        for ch in &mut self.input {
            ch.drain(..n);
        }
        self.input_base += n as u64;
    }

    fn reset_wsola(&mut self) {
        self.output_time = 0.0;
        self.search_block_index = 0;
        self.target_block_index = 0;
        for ch in &mut self.wsola_output {
            ch.fill(0.0);
        }
        self.num_complete_frames = 0;
    }

    fn choose_mode(&self, rate: f64) -> Mode {
        let ola = self.ola_window_size as f64;
        let slower_step = (ola * rate).ceil();
        let faster_step = (ola / rate).ceil();
        if ola <= faster_step && slower_step >= ola {
            Mode::Passthrough
        } else {
            Mode::Wsola
        }
    }

    fn set_mode(&mut self, mode: Mode, rate: f64) {
        let rate_changed = mode == Mode::Wsola && self.run_rate != rate;
        if self.mode == Some(mode) && !rate_changed {
            return;
        }
        if self.mode == Some(Mode::Wsola) {
            self.reset_wsola();
            self.align_to_consumed();
        }
        self.run_rate = rate;
        self.mode = Some(mode);
    }

    /// Renders every output frame the queued input allows at `rate`, interleaved into `out`.
    pub fn process(&mut self, rate: f64, out: &mut Vec<f32>) {
        let mode = self.choose_mode(rate);
        self.set_mode(mode, rate);
        match mode {
            Mode::Passthrough => {
                let frames = self.queued_frames();
                out.reserve(frames * self.channels);
                for n in 0..frames {
                    for ch in 0..self.channels {
                        out.push(self.input[ch][n]);
                    }
                }
                for ch in &mut self.input {
                    ch.clear();
                }
                self.input_base += frames as u64;
                self.consumed += frames as f64;
            }
            Mode::Wsola => loop {
                self.write_completed_frames(out, rate);
                if !self.run_one_wsola_iteration(rate) {
                    break;
                }
            },
        }
    }

    fn write_completed_frames(&mut self, out: &mut Vec<f32>, rate: f64) {
        let n = self.num_complete_frames;
        if n == 0 {
            return;
        }
        self.consumed += n as f64 * rate;
        out.reserve(n * self.channels);
        for i in 0..n {
            for ch in 0..self.channels {
                out.push(self.wsola_output[ch][i]);
            }
        }
        for ch in &mut self.wsola_output {
            ch.copy_within(n.., 0);
        }
        self.num_complete_frames = 0;
    }

    fn can_perform_wsola(&self) -> bool {
        let search_block_size = (self.num_candidate_blocks + self.ola_window_size - 1) as i64;
        let frames = self.queued_frames() as i64;
        self.target_block_index + self.ola_window_size as i64 <= frames
            && self.search_block_index + search_block_size <= frames
    }

    fn run_one_wsola_iteration(&mut self, rate: f64) -> bool {
        if !self.can_perform_wsola() {
            return false;
        }
        self.get_optimal_block();
        let hop = self.ola_hop_size;
        let start = self.num_complete_frames;
        for ch in 0..self.channels {
            let opt = &self.optimal_block[ch];
            let output = &mut self.wsola_output[ch][start..];
            for n in 0..hop {
                output[n] = output[n] * self.ola_window[hop + n] + opt[n] * self.ola_window[n];
            }
            output[hop..2 * hop].copy_from_slice(&opt[hop..2 * hop]);
        }
        self.num_complete_frames += hop;
        self.update_output_time(rate, hop as f64);
        self.remove_old_input_frames(rate);
        true
    }

    fn update_output_time(&mut self, rate: f64, time_change: f64) {
        self.output_time += time_change;
        let search_block_center_index = (self.output_time * rate + 0.5) as i64;
        self.search_block_index = search_block_center_index - self.search_block_center_offset;
    }

    fn remove_old_input_frames(&mut self, rate: f64) {
        let earliest = self.target_block_index.min(self.search_block_index);
        if earliest <= 0 {
            return;
        }
        for ch in &mut self.input {
            ch.drain(..earliest as usize);
        }
        self.input_base += earliest as u64;
        self.target_block_index -= earliest;
        self.update_output_time(rate, -(earliest as f64) / rate);
    }

    fn target_is_within_search_region(&self) -> bool {
        let search_block_size = (self.num_candidate_blocks + self.ola_window_size - 1) as i64;
        self.target_block_index >= self.search_block_index
            && self.target_block_index + self.ola_window_size as i64 <= self.search_block_index + search_block_size
    }

    fn get_optimal_block(&mut self) {
        let optimal_index;
        if self.target_is_within_search_region() {
            optimal_index = self.target_block_index;
            peek_with_zero_prepend(&self.input, optimal_index, &mut self.optimal_block);
        } else {
            peek_with_zero_prepend(&self.input, self.target_block_index, &mut self.target_block);
            peek_with_zero_prepend(&self.input, self.search_block_index, &mut self.search_block);
            let last_optimal = self.target_block_index - self.ola_hop_size as i64 - self.search_block_index;
            let exclude = (last_optimal - EXCLUDE_INTERVAL_FRAMES / 2, last_optimal + EXCLUDE_INTERVAL_FRAMES / 2);
            optimal_index =
                optimal_index_in(&self.search_block, &self.target_block, exclude) as i64 + self.search_block_index;
            peek_with_zero_prepend(&self.input, optimal_index, &mut self.optimal_block);
            let w = self.ola_window_size;
            for ch in 0..self.channels {
                for n in 0..w {
                    self.optimal_block[ch][n] = self.optimal_block[ch][n] * self.transition_window[n]
                        + self.target_block[ch][n] * self.transition_window[w + n];
                }
            }
        }
        self.target_block_index = optimal_index + self.ola_hop_size as i64;
    }
}

fn peek_with_zero_prepend(input: &Bus, offset: i64, dest: &mut Bus) {
    let frames = dest[0].len();
    let zeros = if offset < 0 { (-offset as usize).min(frames) } else { 0 };
    let from = offset.max(0) as usize;
    for (src, dst) in input.iter().zip(dest.iter_mut()) {
        dst[..zeros].fill(0.0);
        dst[zeros..].copy_from_slice(&src[from..from + frames - zeros]);
    }
}

/// Chromium compares a `size_t` index against `int` bounds, so a negative bound wraps to
/// a huge unsigned value; the same conversion is kept here.
fn in_interval(n: usize, q: (i64, i64)) -> bool {
    n >= q.0 as usize && n <= q.1 as usize
}

fn dot(a: &Bus, off_a: usize, b: &Bus, off_b: usize, frames: usize, out: &mut [f32]) {
    for (ch, o) in out.iter_mut().enumerate() {
        let (x, y) = (&a[ch][off_a..off_a + frames], &b[ch][off_b..off_b + frames]);
        *o = x.iter().zip(y).map(|(p, q)| p * q).sum();
    }
}

fn similarity(dot_ab: &[f32], energy_a: &[f32], energy_b: &[f32]) -> f32 {
    const EPSILON: f32 = 1e-12;
    dot_ab.iter().zip(energy_a).zip(energy_b).map(|((d, ea), eb)| d / (ea * eb + EPSILON).sqrt()).sum()
}

fn moving_block_energies(input: &Bus, frames_per_block: usize) -> Vec<f32> {
    let channels = input.len();
    let num_blocks = input[0].len() - (frames_per_block - 1);
    let mut energy = vec![0.0f32; num_blocks * channels];
    for (k, ch) in input.iter().enumerate() {
        energy[k] = ch[..frames_per_block].iter().map(|s| s * s).sum();
        for n in 1..num_blocks {
            let out = ch[n - 1];
            let inn = ch[n - 1 + frames_per_block];
            energy[k + n * channels] = energy[k + (n - 1) * channels] - out * out + inn * inn;
        }
    }
    energy
}

fn quadratic_interpolation(y: [f32; 3]) -> (f32, f32) {
    let a = 0.5 * (y[2] + y[0]) - y[1];
    let b = 0.5 * (y[2] - y[0]);
    let c = y[1];
    if a == 0.0 {
        (0.0, y[1])
    } else {
        let ext = -b / (2.0 * a);
        (ext, a * ext * ext + b * ext + c)
    }
}

fn decimated_search(
    decimation: usize,
    exclude: (i64, i64),
    target: &Bus,
    search: &Bus,
    energy_target: &[f32],
    energy_candidates: &[f32],
) -> usize {
    let channels = search.len();
    let block_size = target[0].len();
    let num_candidate_blocks = search[0].len() - (block_size - 1);
    let mut dot_prod = vec![0.0f32; channels];
    let mut sim = [0.0f32; 3];
    let cand = |n: usize| &energy_candidates[n * channels..(n + 1) * channels];

    let mut n = 0;
    dot(target, 0, search, n, block_size, &mut dot_prod);
    sim[0] = similarity(&dot_prod, energy_target, cand(n));
    let mut best_similarity = sim[0];
    let mut optimal_index = 0;

    n += decimation;
    if n >= num_candidate_blocks {
        return 0;
    }
    dot(target, 0, search, n, block_size, &mut dot_prod);
    sim[1] = similarity(&dot_prod, energy_target, cand(n));

    n += decimation;
    if n >= num_candidate_blocks {
        return if sim[1] > sim[0] { decimation } else { 0 };
    }
    while n < num_candidate_blocks {
        dot(target, 0, search, n, block_size, &mut dot_prod);
        sim[2] = similarity(&dot_prod, energy_target, cand(n));
        if (sim[1] > sim[0] && sim[1] >= sim[2]) || (sim[1] >= sim[0] && sim[1] > sim[2]) {
            let (normalized, candidate_similarity) = quadratic_interpolation(sim);
            let candidate_index =
                (n as i64 - decimation as i64 + (normalized * decimation as f32 + 0.5) as i64) as usize;
            if candidate_similarity > best_similarity && !in_interval(candidate_index, exclude) {
                optimal_index = candidate_index;
                best_similarity = candidate_similarity;
            }
        } else if n + decimation >= num_candidate_blocks && sim[2] > best_similarity && !in_interval(n, exclude) {
            optimal_index = n;
            best_similarity = sim[2];
        }
        sim[0] = sim[1];
        sim[1] = sim[2];
        n += decimation;
    }
    optimal_index
}

fn full_search(
    low: usize,
    high: usize,
    exclude: (i64, i64),
    target: &Bus,
    search: &Bus,
    energy_target: &[f32],
    energy_candidates: &[f32],
) -> usize {
    let channels = search.len();
    let block_size = target[0].len();
    let mut dot_prod = vec![0.0f32; channels];
    let mut best_similarity = f32::MIN_POSITIVE;
    let mut optimal_index = 0;
    for n in low..=high {
        if in_interval(n, exclude) {
            continue;
        }
        dot(target, 0, search, n, block_size, &mut dot_prod);
        let s = similarity(&dot_prod, energy_target, &energy_candidates[n * channels..(n + 1) * channels]);
        if s > best_similarity {
            best_similarity = s;
            optimal_index = n;
        }
    }
    optimal_index
}

fn optimal_index_in(search: &Bus, target: &Bus, exclude: (i64, i64)) -> usize {
    let channels = search.len();
    let target_size = target[0].len();
    let num_candidate_blocks = search[0].len() - (target_size - 1);
    let energy_candidates = moving_block_energies(search, target_size);
    let mut energy_target = vec![0.0f32; channels];
    dot(target, 0, target, 0, target_size, &mut energy_target);
    let optimal = decimated_search(SEARCH_DECIMATION, exclude, target, search, &energy_target, &energy_candidates);
    let low = optimal.saturating_sub(SEARCH_DECIMATION);
    let high = (num_candidate_blocks - 1).min(optimal + SEARCH_DECIMATION);
    full_search(low, high, exclude, target, search, &energy_target, &energy_candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(freq: f32, secs: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        (0..n)
            .flat_map(|i| {
                let s = (2.0 * std::f32::consts::PI * freq * i as f32 / RATE as f32).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    fn render(input: &[f32], rate: f64) -> Vec<f32> {
        let mut w = Wsola::new(2, RATE);
        let mut out = Vec::new();
        for chunk in input.chunks(2 * 1024) {
            w.push_interleaved(chunk);
            w.process(rate, &mut out);
        }
        out
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.chunks_exact(2).map(|f| f[0]).collect::<Vec<_>>().windows(2).filter(|p| p[0] < 0.0 && p[1] >= 0.0).count()
    }

    #[test]
    fn audio_at_unit_rate_is_unchanged() {
        let input = sine(440.0, 1.0);
        assert_eq!(render(&input, 1.0), input);
    }

    #[test]
    fn double_rate_halves_the_duration_of_the_audio() {
        let input = sine(440.0, 2.0);
        let out = render(&input, 2.0);
        let frames = out.len() as f64 / 2.0;
        assert!((frames - 48_000.0).abs() < 2_000.0, "{frames} frames");
    }

    #[test]
    fn double_rate_does_not_change_the_pitch() {
        let out = render(&sine(3_000.0, 2.0), 2.0);
        let secs = out.len() as f64 / 2.0 / RATE as f64;
        let hz = zero_crossings(&out) as f64 / secs;
        assert!((hz - 3_000.0).abs() < 30.0, "{hz} Hz");
    }

    /// Renders a click every 250 ms through rate toggles and returns, per click, how far
    /// its output position is from the map the tempo element reports (each change at the
    /// consumed input and the emitted output at that moment), in ms.
    fn click_errors(schedule: impl Fn(usize) -> f64) -> Vec<f64> {
        let (chunk, secs) = (1024usize, 12usize);
        let total = RATE as usize * secs;
        let period = RATE as usize / 4;
        let mut input = vec![0.0f32; total * 2];
        for c in (period..total).step_by(period) {
            input[c * 2] = 1.0;
            input[c * 2 + 1] = 1.0;
        }
        let mut w = Wsola::new(2, RATE);
        let mut out = Vec::new();
        // (input frame where a rate starts, output frame where it starts, rate)
        let mut map: Vec<(f64, f64, f64)> = Vec::new();
        for (i, block) in input.chunks(chunk * 2).enumerate() {
            let rate = schedule(i);
            if map.last().is_none_or(|m| m.2 != rate) {
                map.push((w.consumed(), (out.len() / 2) as f64, rate));
            }
            w.push_interleaved(block);
            w.process(rate, &mut out);
        }
        let expect = |c: f64| {
            let &(mc, mo, mr) = map.iter().rev().find(|m| c >= m.0).unwrap();
            mo + (c - mc) / mr
        };
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0].abs()).collect();
        let mut errors = Vec::new();
        for c in (period..total).step_by(period) {
            let e = expect(c as f64) as i64;
            let (lo, hi) = ((e - 2_400).max(0) as usize, ((e + 2_400) as usize).min(left.len()));
            if lo >= hi {
                break;
            }
            let (k, v) = left[lo..hi].iter().enumerate().fold((0, 0.0f32), |b, (k, &v)| if v > b.1 { (k, v) } else { b });
            if v > 0.3 {
                errors.push(((lo + k) as f64 - e as f64) / RATE as f64 * 1000.0);
            }
        }
        errors
    }

    #[test]
    fn quick_rate_changes_do_not_drift_audio_off_the_reported_timeline() {
        // 20 short 2x blips: 2 chunks (~43 ms) of 2x every 20 chunks.
        let errors = click_errors(|i| if i % 20 < 2 { 2.0 } else { 1.0 });
        let worst = errors.iter().fold(0.0f64, |m, e| m.max(e.abs()));
        let last = *errors.last().unwrap();
        assert!(worst < 5.0, "worst {worst:.1} ms, last {last:.1} ms, all {errors:?}");
    }

}
