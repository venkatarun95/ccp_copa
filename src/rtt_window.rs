use std;
use std::collections::{VecDeque};

pub struct RTTWindow {
    // Maximum time till which to maintain history. It is minimum of 10s and 20
    // RTTs.
    max_time: u64,
    // Minimum RTT in current sample
    min_rtt: u32,

    // RTT measurements
    rtts: VecDeque<u32>,
    // Times at which the measurements were reported
    times: VecDeque<u64>,

    // Whether or not RTT has increased in the last 2 X min. RTT period along
    // with the ending time of that period
    increase: VecDeque<(u64, bool)>,
    // Minimum RTT between now and now-2*min_rtt
    cur_min_rtt: u32,
    // Minimum RTT between now-2*min_rtt and now-4*min_rtt
    prev_min_rtt: u32,
    // Number of increases and decreases in the current `increase` window
    num_increase: u32,
    num_decrease: u32,
}

impl RTTWindow {
    pub fn new() -> Self {
        Self {
            max_time: 10_000_000,
            min_rtt: std::u32::MAX,

            rtts: VecDeque::new(),
            times: VecDeque::new(),

            increase: VecDeque::new(),
            cur_min_rtt: std::u32::MAX,
            prev_min_rtt: 0, // We want to bias toward TCP mode
            num_increase: 0,
            num_decrease: 0,
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

        // If necessary, recompute min rtt
        if recompute_min_rtt {
            self.min_rtt = std::u32::MAX;
            for x in self.rtts.iter() {
                if *x < self.min_rtt {
                    self.min_rtt = *x;
                }
            }
            assert!(self.min_rtt != std::u32::MAX);
        }

        // Delete all old increase/decrease samples
        while self.increase.len() > 40 {
            let increase: bool = self.increase.front().unwrap().1;
            if increase {self.num_increase -= 1;}
            else {self.num_decrease -= 1;}
            self.increase.pop_front();
        }
    }

    pub fn get_min_rtt(&self) -> u32 {
        self.min_rtt
    }

    pub fn new_rtt_sample(&mut self, rtt: u32, now: u64) {
        assert!(self.rtts.len() == self.times.len());
        println!("Measurement rtt: {}, min_rtt: {}", rtt, self.min_rtt);
        self.max_time = std::cmp::max(10_000_000, 30 * rtt as u64);

        // Push back data
        self.rtts.push_back(rtt);
        self.times.push_back(now);

        // Update min. RTT
        if rtt < self.min_rtt {
            self.min_rtt = rtt;
        }

        // Update increase
        if self.increase.len() == 0 ||
            self.increase.back().unwrap().0 < now - 2 * self.min_rtt as u64 {
                let increase = self.cur_min_rtt > self.prev_min_rtt;
                self.increase.push_back((now, increase));
                self.prev_min_rtt = self.cur_min_rtt;
                self.cur_min_rtt = std::u32::MAX;
                if increase {self.num_increase += 1;}
                else {self.num_decrease += 1;}
            }
        self.cur_min_rtt = std::cmp::min(self.cur_min_rtt, rtt);

        // Delete old data
        self.clear_old_hist(now);

        self.max_time = std::cmp::min(10_000_000, 20 * self.min_rtt as u64);
    }

    pub fn tcp_detected(&mut self) -> bool {
        if self.rtts.len() == 0 {
            return false;
        }

        let mut min1 = std::u32::MAX;
        let mut max = 0;

        for i in 0..(self.rtts.len()) {
            if self.times[i] >
                self.times.back().unwrap() - self.min_rtt as u64*8 {
                    min1 = std::cmp::min(min1, self.rtts[i]);
                    max = std::cmp::max(max, self.rtts[i]);
                }
            // else if self.times[i] >
            //     self.times.back().unwrap() - self.min_rtt as u64*8 {
            //         min2 = std::cmp::min(min2, self.rtts[i]);
            //         max = std::cmp::max(max, self.rtts[i]);
            //     }
        }

        let thresh = self.min_rtt + (max - self.min_rtt) / 10 + 100;
        println!("min1 = {}, max = {}, thresh = {}", min1, max, thresh);
        let res = min1 > thresh;
        res
    }

    pub fn num_tcp_detect_samples(&self) -> u32 {
        self.num_increase + self.num_decrease
    }
}
