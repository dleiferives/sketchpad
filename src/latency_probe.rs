use sketchpad::input::{TabletPhase, TabletSample};
use std::time::{Duration, Instant};

const SAMPLE_CAPACITY: usize = 2_048;

pub(crate) struct LatencySeries {
    values: [u64; SAMPLE_CAPACITY],
    retained: usize,
    next: usize,
    count: u64,
    total: u64,
    maximum: u64,
}

impl LatencySeries {
    pub(crate) fn new() -> Self {
        Self {
            values: [0; SAMPLE_CAPACITY],
            retained: 0,
            next: 0,
            count: 0,
            total: 0,
            maximum: 0,
        }
    }

    pub(crate) fn record(&mut self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self.record_value(micros);
    }

    fn record_value(&mut self, value: u64) {
        self.values[self.next] = value;
        self.next = (self.next + 1) % SAMPLE_CAPACITY;
        self.retained = (self.retained + 1).min(SAMPLE_CAPACITY);
        self.count += 1;
        self.total = self.total.saturating_add(value);
        self.maximum = self.maximum.max(value);
    }

    pub(crate) fn summary(&self) -> LatencySummary {
        if self.count == 0 {
            return LatencySummary::default();
        }
        let mut retained = self.values[..self.retained].to_vec();
        retained.sort_unstable();
        let p95_index = ((retained.len() * 95).div_ceil(100) - 1).min(retained.len() - 1);
        LatencySummary {
            count: self.count,
            mean: self.total / self.count,
            p95: retained[p95_index],
            maximum: self.maximum,
        }
    }

    pub(crate) fn has_samples(&self) -> bool {
        self.count > 0
    }

    pub(crate) fn clear(&mut self) {
        self.retained = 0;
        self.next = 0;
        self.count = 0;
        self.total = 0;
        self.maximum = 0;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LatencySummary {
    pub(crate) count: u64,
    pub(crate) mean: u64,
    pub(crate) p95: u64,
    pub(crate) maximum: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabletActivity {
    Hover,
    Contact,
}

impl TabletActivity {
    fn from_phase(phase: TabletPhase) -> Self {
        if phase == TabletPhase::Hover {
            Self::Hover
        } else {
            Self::Contact
        }
    }
}

struct TabletPhaseLatency {
    source_excess: LatencySeries,
    backend_to_handler: LatencySeries,
    latest_to_submit: LatencySeries,
}

impl TabletPhaseLatency {
    fn new() -> Self {
        Self {
            source_excess: LatencySeries::new(),
            backend_to_handler: LatencySeries::new(),
            latest_to_submit: LatencySeries::new(),
        }
    }

    fn summary(&self) -> TabletPhaseLatencySummary {
        TabletPhaseLatencySummary {
            source_excess: self.source_excess.summary(),
            backend_to_handler: self.backend_to_handler.summary(),
            latest_to_submit: self.latest_to_submit.summary(),
        }
    }

    fn clear(&mut self) {
        self.source_excess.clear();
        self.backend_to_handler.clear();
        self.latest_to_submit.clear();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabletPhaseLatencySummary {
    pub(crate) source_excess: LatencySummary,
    pub(crate) backend_to_handler: LatencySummary,
    pub(crate) latest_to_submit: LatencySummary,
}

struct SourceClockAlignment {
    first_source_millis: Option<u64>,
    first_received_at: Option<Instant>,
    minimum_offset_micros: i128,
}

impl SourceClockAlignment {
    fn new() -> Self {
        Self {
            first_source_millis: None,
            first_received_at: None,
            minimum_offset_micros: i128::MAX,
        }
    }

    fn relative_excess(&mut self, source_millis: u64, received_at: Instant) -> Duration {
        let first_source_millis = *self.first_source_millis.get_or_insert(source_millis);
        let first_received_at = *self.first_received_at.get_or_insert(received_at);
        let source_elapsed_micros =
            i128::from(source_millis.saturating_sub(first_source_millis)) * 1_000;
        let received_elapsed_micros = received_at
            .saturating_duration_since(first_received_at)
            .as_micros()
            .min(i128::MAX as u128) as i128;
        let offset_micros = received_elapsed_micros - source_elapsed_micros;
        self.minimum_offset_micros = self.minimum_offset_micros.min(offset_micros);
        let excess_micros = (offset_micros - self.minimum_offset_micros)
            .max(0)
            .min(u64::MAX as i128) as u64;
        Duration::from_micros(excess_micros)
    }
}

#[derive(Clone, Copy)]
struct PendingTabletFrame {
    latest_handled_at: Instant,
    activity: TabletActivity,
    sample_count: u64,
}

pub(crate) struct TabletLatencyMetrics {
    source_clock: SourceClockAlignment,
    hover: TabletPhaseLatency,
    contact: TabletPhaseLatency,
    pending_frame: Option<PendingTabletFrame>,
    samples_per_submit: LatencySeries,
}

impl TabletLatencyMetrics {
    pub(crate) fn new() -> Self {
        Self {
            source_clock: SourceClockAlignment::new(),
            hover: TabletPhaseLatency::new(),
            contact: TabletPhaseLatency::new(),
            pending_frame: None,
            samples_per_submit: LatencySeries::new(),
        }
    }

    fn phase_mut(&mut self, activity: TabletActivity) -> &mut TabletPhaseLatency {
        match activity {
            TabletActivity::Hover => &mut self.hover,
            TabletActivity::Contact => &mut self.contact,
        }
    }

    pub(crate) fn observe_sample(
        &mut self,
        phase: TabletPhase,
        sample: TabletSample,
        backend_received_at: Instant,
        handled_at: Instant,
    ) {
        let activity = TabletActivity::from_phase(phase);
        let source_excess = self
            .source_clock
            .relative_excess(sample.timestamp_millis, backend_received_at);
        let backend_to_handler = handled_at.saturating_duration_since(backend_received_at);
        let phase_metrics = self.phase_mut(activity);
        phase_metrics.source_excess.record(source_excess);
        phase_metrics.backend_to_handler.record(backend_to_handler);

        self.pending_frame = Some(PendingTabletFrame {
            latest_handled_at: handled_at,
            activity,
            sample_count: self
                .pending_frame
                .map_or(1, |pending| pending.sample_count.saturating_add(1)),
        });
    }

    pub(crate) fn observe_submit(&mut self, submitted_at: Instant) {
        let Some(pending) = self.pending_frame.take() else {
            return;
        };
        self.phase_mut(pending.activity)
            .latest_to_submit
            .record(submitted_at.saturating_duration_since(pending.latest_handled_at));
        self.samples_per_submit.record_value(pending.sample_count);
    }

    pub(crate) fn summary(&self) -> TabletLatencySummary {
        TabletLatencySummary {
            hover: self.hover.summary(),
            contact: self.contact.summary(),
            samples_per_submit: self.samples_per_submit.summary(),
        }
    }

    pub(crate) fn clear_period(&mut self) {
        self.hover.clear();
        self.contact.clear();
        self.samples_per_submit.clear();
    }
}

pub(crate) struct FrameStageMetrics {
    acquire: LatencySeries,
    prepare: LatencySeries,
    encode: LatencySeries,
    submit: LatencySeries,
}

impl FrameStageMetrics {
    pub(crate) fn new() -> Self {
        Self {
            acquire: LatencySeries::new(),
            prepare: LatencySeries::new(),
            encode: LatencySeries::new(),
            submit: LatencySeries::new(),
        }
    }

    pub(crate) fn record(
        &mut self,
        acquire: Duration,
        prepare: Duration,
        encode: Duration,
        submit: Duration,
    ) {
        self.acquire.record(acquire);
        self.prepare.record(prepare);
        self.encode.record(encode);
        self.submit.record(submit);
    }

    pub(crate) fn summary(&self) -> FrameStageSummary {
        FrameStageSummary {
            acquire: self.acquire.summary(),
            prepare: self.prepare.summary(),
            encode: self.encode.summary(),
            submit: self.submit.summary(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.acquire.clear();
        self.prepare.clear();
        self.encode.clear();
        self.submit.clear();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameStageSummary {
    pub(crate) acquire: LatencySummary,
    pub(crate) prepare: LatencySummary,
    pub(crate) encode: LatencySummary,
    pub(crate) submit: LatencySummary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabletLatencySummary {
    pub(crate) hover: TabletPhaseLatencySummary,
    pub(crate) contact: TabletPhaseLatencySummary,
    pub(crate) samples_per_submit: LatencySummary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sketchpad::input::ToolKind;

    fn sample(timestamp_millis: u64) -> TabletSample {
        TabletSample {
            device_id: 1,
            tool: ToolKind::Pen,
            position: [10.0, 20.0],
            pressure: 0.5,
            tilt: [0.0, 0.0],
            distance: 0.0,
            timestamp_millis,
        }
    }

    #[test]
    fn latency_series_reports_distribution_and_resets() {
        let mut series = LatencySeries::new();
        for micros in 1..=100 {
            series.record(Duration::from_micros(micros));
        }
        assert_eq!(
            series.summary(),
            LatencySummary {
                count: 100,
                mean: 50,
                p95: 95,
                maximum: 100,
            }
        );

        series.clear();
        assert_eq!(series.summary(), LatencySummary::default());
    }

    #[test]
    fn source_clock_alignment_reports_excess_over_best_observed_delivery() {
        let start = Instant::now();
        let mut alignment = SourceClockAlignment::new();

        assert_eq!(
            alignment.relative_excess(1_000, start),
            Duration::from_micros(0)
        );
        assert_eq!(
            alignment.relative_excess(1_010, start + Duration::from_millis(12)),
            Duration::from_millis(2)
        );
        assert_eq!(
            alignment.relative_excess(1_020, start + Duration::from_millis(21)),
            Duration::from_millis(1)
        );
        assert_eq!(
            alignment.relative_excess(1_030, start + Duration::from_millis(30)),
            Duration::from_micros(0)
        );
    }

    #[test]
    fn frame_stage_metrics_keep_boundaries_independent() {
        let mut metrics = FrameStageMetrics::new();
        metrics.record(
            Duration::from_millis(10),
            Duration::from_millis(2),
            Duration::from_millis(3),
            Duration::from_millis(1),
        );

        let summary = metrics.summary();
        assert_eq!(summary.acquire.mean, 10_000);
        assert_eq!(summary.prepare.mean, 2_000);
        assert_eq!(summary.encode.mean, 3_000);
        assert_eq!(summary.submit.mean, 1_000);

        metrics.clear();
        assert_eq!(metrics.summary(), FrameStageSummary::default());
    }

    #[test]
    fn tablet_latency_keeps_hover_and_contact_paths_separate() {
        let start = Instant::now();
        let mut metrics = TabletLatencyMetrics::new();

        metrics.observe_sample(
            TabletPhase::Hover,
            sample(1_000),
            start,
            start + Duration::from_millis(2),
        );
        metrics.observe_submit(start + Duration::from_millis(5));
        metrics.observe_sample(
            TabletPhase::Move,
            sample(1_010),
            start + Duration::from_millis(10),
            start + Duration::from_millis(14),
        );
        metrics.observe_submit(start + Duration::from_millis(20));

        let summary = metrics.summary();
        assert_eq!(summary.hover.backend_to_handler.count, 1);
        assert_eq!(summary.hover.backend_to_handler.mean, 2_000);
        assert_eq!(summary.hover.latest_to_submit.mean, 3_000);
        assert_eq!(summary.contact.backend_to_handler.count, 1);
        assert_eq!(summary.contact.backend_to_handler.mean, 4_000);
        assert_eq!(summary.contact.latest_to_submit.mean, 6_000);
        assert_eq!(summary.samples_per_submit.count, 2);
        assert_eq!(summary.samples_per_submit.mean, 1);
    }
}
