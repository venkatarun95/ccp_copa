use rtt_window::RTTWindow;

#[derive(Clone, Eq, PartialEq)]
pub enum DeltaModeConf {NoTCP, Auto}

#[derive(Clone, Eq, PartialEq)]
pub enum DeltaMode {Default, TCPCoop, Loss}

pub struct DeltaManager {
    // Configuration on how to choose delta
    switch_mode: DeltaModeConf,
    default_delta: f32,
    // End of the last window of tracking losses
    prev_loss_cycle: u64,
    // Loss rate in the previous cycle
    prev_loss_rate: f32,
    // Number of acks and losses in current cycle
    cur_num_acked: u32,
    cur_num_losses: u32,
    // Last time we reduced 1/delta due to loss, so we don't decrease twice
    // within the same RTT
    prev_loss_red_time: u64,
    // Current state of delta
    cur_mode: DeltaMode,
    delta: f32,
}

impl DeltaManager {
    pub fn new(default_delta: f32, mode: DeltaModeConf) -> Self {
        let cur_mode = match mode {
            DeltaModeConf::NoTCP => DeltaMode::Default,
            DeltaModeConf::Auto => DeltaMode::TCPCoop,
        };
        if default_delta > 1.0 {
            panic!("Default delta should be less than or equal to 1.");
        }
        Self {
            switch_mode: mode,
            default_delta: default_delta,
            prev_loss_cycle: 0,
            prev_loss_rate: 0.,
            cur_num_acked: 0,
            cur_num_losses: 0,
            prev_loss_red_time: 0,
            cur_mode: cur_mode,
            delta: 0.5,
        }
    }

    pub fn report_measurement(&mut self, rtt_win: &mut RTTWindow, acked: u32,
                              lost: u32, now: u64) {
        // Update loss rate estimate
        self.cur_num_acked += acked;
        self.cur_num_losses += lost;
        if now > self.prev_loss_cycle + 2 * rtt_win.get_min_rtt() as u64 {
            self.prev_loss_cycle = now;
            if self.cur_num_losses + self.cur_num_acked > 0 {
                self.prev_loss_rate = self.cur_num_losses as f32 /
                    (self.cur_num_losses + self.cur_num_acked) as f32;
            }
            self.cur_num_acked = 0;
            self.cur_num_losses = 0;
        }

        // Set delta mode
        // If we are losing more than 10% of packets, move to loss mode. Period.
        if self.prev_loss_rate >= 0.1 {
            self.cur_mode = DeltaMode::Loss;
        }
        else {
            // See if we need to be in TCP mode
            if self.switch_mode == DeltaModeConf::Auto &&
                (rtt_win.num_tcp_detect_samples() < 10 ||
                 rtt_win.tcp_detected()) {
                    self.cur_mode = DeltaMode::TCPCoop;
            }
            else {
                self.cur_mode = DeltaMode::Default;
                self.delta = self.default_delta;
            }
        }

        // Set delta
        match self.cur_mode {
            DeltaMode::Default => {
                // Coming from TCPCoop mode
                self.delta = self.default_delta;
            }
            DeltaMode::TCPCoop => {
                if lost > 0 {
                    if now - rtt_win.get_min_rtt() as u64 >
                        self.prev_loss_red_time {
                            self.delta *= 2.;
                            self.prev_loss_red_time = now;
                    }
                }
                else {
                    self.delta = 1. / (1. + 1. / self.delta);
                }
                if self.delta > self.default_delta {
                    self.delta = self.default_delta;
                }
            },
            DeltaMode::Loss => {
                if lost > 0 {
                    self.delta *= 2.;
                }
                if self.delta >= self.default_delta {
                    self.delta = self.default_delta;
                }
            }
        };
    }

    pub fn get_delta(&self) -> f32 {
        self.delta
    }

    pub fn get_mode(&self) -> DeltaMode {
        self.cur_mode.clone()
    }
}
