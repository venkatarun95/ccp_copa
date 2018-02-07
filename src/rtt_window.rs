use std;
use std::collections::{VecDeque, HashMap};

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
    // Cached results of binomial test coefficients (n, k) -> probability
    binom_cache: HashMap<(u32, u32), f32>,
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
            binom_cache: HashMap::new(),
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
        while self.increase.len() > 30 {
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
        let tcp = self.tcp_detected();
        println!("num_increase = {}, num_decrease = {}, cur_min_rtt = {}, prev_min_rtt = {}, tcp = {}",
                 self.num_increase, self.num_decrease, self.cur_min_rtt, self.prev_min_rtt, tcp);
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

    // Return probability that in n tosses of a fair coin, <= k will be tails.
    // Result is cached for efficiency
    fn get_binom_test(&mut self, n: u32, mut k: u32) -> f32 {
        if k > n/2 {
            k = n-k;
        }
        if self.binom_cache.contains_key(&(n, k)) {
            return self.binom_cache.get(&(n, k)).unwrap().clone();
        }
        let mut num = 1f64;
        let mut inv_fact = 1f64;
        let mut denom = 1f64;
        for i in 0..k {
            if i <= k {
                inv_fact *= (n-i) as f64;
                denom *= (i+1) as f64;
                num += inv_fact/denom;
            }
        }
        let res = (num) / (1 << n) as f64;
        self.binom_cache.insert((n, k), res as f32);
        res as f32
    }

    pub fn tcp_detected(&mut self) -> bool {
        let k = self.num_increase;
        let n = self.num_increase + self.num_decrease;
        println!("n = {}, k = {}, p = {}", n, k, self.get_binom_test(n, k));
        self.get_binom_test(n, k) <= 0.05
    }

    pub fn num_tcp_detect_samples(&self) -> u32 {
        self.num_increase + self.num_decrease
    }
}
