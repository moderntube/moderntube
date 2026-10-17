//! Content time to output time across live tempo changes.
use std::sync::Mutex;

/// Content time to output time while the tempo stage changes speed live: one breakpoint per
/// speed change since the last seek. The tempo stage stamps the audio; video is stamped here.
#[derive(Default)]
pub(super) struct Stretch {
    pub(super) bps: Mutex<Vec<(u64, u64, f64)>>,
    /// When the latest speed change was asked for, until the tempo stage applies it.
    pub(super) requested: Mutex<Option<std::time::Instant>>,
    /// The latest change's content time and when it was asked for, until video reaches it.
    pub(super) pending: Mutex<Option<(u64, std::time::Instant)>>,
}

impl Stretch {
    /// A flush restarts the tempo stage at `content`, scaled by the tempo it runs at.
    pub(super) fn reset(&self, content: u64, rate: f64) {
        *self.bps.lock().unwrap() = vec![(content, (content as f64 / rate) as u64, rate)];
    }

    pub(super) fn segment_scale(&self) -> f64 {
        self.bps.lock().unwrap().first().map_or(1.0, |b| b.2)
    }

    pub(super) fn to_output(&self, content: u64) -> u64 {
        let bps = self.bps.lock().unwrap();
        let Some(&(c0, o0, r)) = bps.iter().rev().find(|b| content >= b.0).or(bps.first()) else {
            return content;
        };
        (o0 as f64 + (content as f64 - c0 as f64) / r).max(0.0) as u64
    }

    pub(super) fn to_content(&self, output: u64) -> u64 {
        let bps = self.bps.lock().unwrap();
        let Some(&(c0, o0, r)) = bps.iter().rev().find(|b| output >= b.1).or(bps.first()) else {
            return output;
        };
        (c0 as f64 + (output as f64 - o0 as f64) * r).max(0.0) as u64
    }

    pub(super) fn request(&self) {
        *self.requested.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// The tempo stage switched to `rate` at content `c0`, output `o0`.
    pub(super) fn change_at(&self, c0: u64, o0: u64, rate: f64) {
        let asked = self.requested.lock().unwrap().take().unwrap_or_else(std::time::Instant::now);
        let mut bps = self.bps.lock().unwrap();
        // Before the first seek the timeline is the open's identity mapping.
        if bps.is_empty() {
            bps.push((0, 0, 1.0));
        }
        bps.push((c0, o0, rate));
        *self.pending.lock().unwrap() = Some((c0, asked));
    }

    /// Logs how long a change took to reach the screen, once a frame at its content time does.
    pub(super) fn note_frame(&self, content: u64) {
        let mut pending = self.pending.lock().unwrap();
        if let Some((c0, at)) = *pending {
            if content >= c0 {
                tracing::info!("[gst] speed change on screen after {}ms", at.elapsed().as_millis());
                *pending = None;
            }
        }
    }
}

#[cfg(test)]
mod stretch_tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn frames_before_a_rate_change_keep_their_times_and_later_ones_scale() {
        let st = Stretch::default();
        st.change_at(10 * S, 10 * S, 2.0);
        assert_eq!(st.to_output(8 * S), 8 * S);
        assert_eq!(st.to_output(14 * S), 12 * S);
    }

    #[test]
    fn content_time_and_output_time_round_trip_across_rate_changes() {
        let st = Stretch::default();
        st.change_at(10 * S, 10 * S, 2.0);
        st.change_at(20 * S, 15 * S, 1.0);
        for c in [3 * S, 12 * S, 25 * S] {
            assert_eq!(st.to_content(st.to_output(c)), c);
        }
        assert_eq!(st.to_output(25 * S), 20 * S);
    }

}
