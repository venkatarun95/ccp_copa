use std;
use std::collections::VecDeque;

pub struct RTTWindow {
    // RTT measurements
    rtts: VecDeque<u32>,
    // Times at which the measurements were reported
    times: VecDeque<u64>,
    // Maximum time till which to maintain history. It is minimum of 10s and 20
    // RTTs.
    max_time: u64,
    // Minimum RTT in current sample
    min_rtt: u32
}

impl RTTWindow {
    pub fn new() -> Self {
        Self {
            rtts: VecDeque::new(),
            times: VecDeque::new(),
            max_time: 10_000_000,
            min_rtt: std::u32::MAX,
        }
    }

    fn clear_old_hist(&mut self, now: u64) {
        assert!(self.rtts.len() == self.times.len());
        // Whether or not min. RTT needs to be recomputed
        let mut recompute_min_rtt = false;
        // Delete all samples older than max_time. However, if there is only one
        // sample left, don't delete it
        while self.times.len() > 1 &&
            self.times.front().unwrap() < &(now - self.max_time) {
                if self.rtts.front().unwrap() <= &self.min_rtt {
                    recompute_min_rtt = true;
                }
                self.times.pop_front();
                self.rtts.pop_front();
            }
        if recompute_min_rtt {
            self.min_rtt = std::u32::MAX;
            for x in self.rtts.iter() {
                if *x < self.min_rtt {
                    self.min_rtt = *x;
                }
            }
            assert!(self.min_rtt != std::u32::MAX);
        }
    }

    pub fn get_min_rtt(&self) -> u32 {
        self.min_rtt
    }

    pub fn new_rtt_sample(&mut self, rtt: u32, now: u64) {
        assert!(self.rtts.len() == self.times.len());
        self.rtts.push_back(rtt);
        self.times.push_back(now);
        if rtt < self.min_rtt {
            self.min_rtt = rtt;
        }
        self.clear_old_hist(now);
        self.max_time = std::cmp::min(10_000_000, 20 * self.min_rtt as u64);
    }
}
