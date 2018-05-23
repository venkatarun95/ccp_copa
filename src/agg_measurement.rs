use std;

use portus::Report;
use portus::lang::Scope;

#[derive(Clone, PartialEq, Eq)]
pub enum ReportStatus {Report, NoReport, UrgentReport}

// CCP may return before the specified time. This struct will aggregate relevant
// values till the time is right
pub struct AggMeasurement {
    // In fraction of a (smoothed) RTT
    reporting_interval: f32,
    // For determining when to report
    srtt: f32,
    // EWMA variable
    srtt_alpha: f32,
    // Last time we reported
    last_report_time: u64,
    // Aggregate variables that are reset every measurement interval
    acked: u32,
    sacked: u32,
    rtt: u32,
    min_rtt: u32,
}

impl AggMeasurement {
    pub fn new(reporting_interval: f32) -> Self {
        Self {
            reporting_interval: reporting_interval,
            srtt: 0.,
            srtt_alpha: 1. / 16.,
            last_report_time: 0,
            acked: 0,
            sacked: 0,
            rtt: 0,
            min_rtt: std::u32::MAX,
        }
    }

    pub fn report(&mut self, m: Report, sc: &Scope) -> (ReportStatus, bool, u32, u32, u32, u32, u32, u32, u64) {
        let acked = m.get_field("Report.acked", sc).expect(
            "expected acked field in returned measurement",
        ) as u32;

        let sacked = m.get_field("Report.sacked", sc).expect(
            "expected sacked field in returned measurement",
        ) as u32;

        let was_timeout = m.get_field("Report.timeout", sc).expect(
            "expected timeout field in returned measurement",
        ) as u32;

        let inflight = m.get_field("Report.inflight", sc).expect(
            "expected inflight field in returned measurement",
        ) as u32;

        let loss = m.get_field("Report.loss", sc).expect(
            "expected loss field in returned measurement",
        ) as u32;

        let rtt = m.get_field("Report.rtt", sc).expect(
            "expected rtt field in returned measurement",
        ) as u32;

        let now = m.get_field("Report.now", sc).expect(
            "expected now field in returned measurement",
        ) as u64;

        let min_rtt = m.get_field("Report.minrtt", sc).expect(
            "expected minrtt field in returned measurement",
        ) as u32;

        self.acked += acked;
        self.sacked = sacked;
        self.min_rtt = std::cmp::min(self.min_rtt, min_rtt);

        if was_timeout == 1 || loss > 0 {
            return (ReportStatus::UrgentReport, (was_timeout == 1), 0, 0, loss,
                    0, 0, 0, now);
        }

        if rtt != 0 {
            self.rtt = rtt;
            self.srtt = self.srtt_alpha * rtt as f32+
                (1. - self.srtt_alpha) * self.srtt;
        }

        if now > 0 && self.last_report_time <
            now - (self.srtt * self.reporting_interval) as u64 {
                let res = (ReportStatus::Report, false, self.acked, self.sacked,
                           loss, inflight, self.rtt, self.min_rtt, now);
                self.last_report_time = now;
                self.acked = 0;
                self.sacked = 0;
                self.rtt = 0;
                self.min_rtt = std::u32::MAX;
                return res;
        }
        else {
            return (ReportStatus::NoReport, false, 0, 0, 0, 0, 0, 0, now);
        }
    }
}
