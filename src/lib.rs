extern crate clap;

#[macro_use]
extern crate slog;
#[macro_use]
extern crate portus;

use portus::{CongAlg, Config, Datapath, DatapathInfo, Measurement};
use portus::pattern;
use portus::ipc::Ipc;
use portus::lang::Scope;

mod rtt_window;
mod delta_manager;
use rtt_window::RTTWindow;
use delta_manager::{DeltaManager, DeltaMode};
pub use delta_manager::DeltaModeConf;
mod agg_measurement;
use agg_measurement::{AggMeasurement, ReportStatus};

pub struct Copa<T: Ipc> {
    control_channel: Datapath<T>,
    logger: Option<slog::Logger>,
    sc: Option<Scope>,
    delta_manager: DeltaManager,
    sock_id: u32,
    cwnd: u32,
    init_cwnd: u32,
    slow_start: bool,
    rtt_win: RTTWindow,
    velocity: u32,
    cur_direction: i64,
    prev_direction: i64,
    prev_update_rtt: u64,
    agg_measurement: AggMeasurement,
}

#[derive(Clone)]
pub struct CopaConfig {
    pub init_cwnd: u32,
    pub default_delta: f32,
    pub delta_mode: DeltaModeConf,
}

impl Default for CopaConfig {
    fn default() -> Self {
        CopaConfig {
            init_cwnd: 0,
            default_delta: 0.5,
            delta_mode: DeltaModeConf::Auto,
        }
    }
}

impl<T: Ipc> Copa<T> {
    fn compute_rate(&self) -> u32 {
        (2 * self.cwnd as u64 * 1000000 / self.rtt_win.get_min_rtt() as u64) as u32
    }

    fn send_pattern(&self) {
        match self.control_channel.send_pattern(
            self.sock_id,
            make_pattern!(
                // In bytes/s
                pattern::Event::SetRateAbs(self.compute_rate()) =>
                pattern::Event::SetCwndAbs(self.cwnd) =>
                pattern::Event::WaitRtts(0.1) => 
                pattern::Event::Report
            ),
        ) {
            Ok(_) => (),
            Err(e) => {
                self.logger.as_ref().map(|log| {
                    warn!(log, "send_pattern"; "err" => ?e);
                });
            }
        }
    }

    fn install_fold(&self) -> Option<Scope> {
        Some(self.control_channel.install_measurement(
            self.sock_id,
            "
                (def (acked 0) (sacked 0) (loss 0) (inflight 0) (timeout false) (rtt 0) (now 0
) (minrtt +infinity))
                (bind Flow.acked (+ Flow.acked Pkt.bytes_acked))
                (bind Flow.inflight Pkt.packets_in_flight)
                (bind Flow.rtt Pkt.rtt_sample_us)
                (bind Flow.minrtt (min Flow.minrtt Pkt.rtt_sample_us))
                (bind Flow.sacked (+ Flow.sacked Pkt.packets_misordered))
                (bind Flow.loss Pkt.lost_pkts_sample)
                (bind Flow.timeout Pkt.was_timeout)
                (bind Flow.now Pkt.now)
                (bind isUrgent Pkt.was_timeout)
                (bind isUrgent (!if isUrgent (> Flow.loss 0)))
            ".as_bytes(),
        ).unwrap())
    }

    // fn create(control: Datapath<T>, cfg: Config<T, Copa<T>>, info: DatapathInfo) -> Self {
    //     let mut s = Self {
    //         control_channel: control,
    //         logger: cfg.logger,
    //         cwnd: info.init_cwnd,
    //         init_cwnd: info.init_cwnd,
    //         sc: None,
    //         delta_manager: DeltaManager::new(cfg.config.default_delta, cfg.config.delta_mode),
    //         sock_id: info.sock_id,
    //         slow_start: true,
    //         rtt_win: RTTWindow::new(),
    //         pkts_in_last_rtt: 0,
    //         velocity: 1,
    //         cur_direction: 0,
    //         prev_direction: 0,
    //         prev_update_rtt: 0,
    //         agg_measurement: AggMeasurement::new(0.5),
    //     };

    //     if cfg.config.init_cwnd != 0 {
    //         s.cwnd = cfg.config.init_cwnd;
    //         s.init_cwnd = cfg.config.init_cwnd;
    //     }

    //     s.logger.as_ref().map(|log| {
    //         info!(log, "starting copa flow"; "sock_id" => info.sock_id);
    //     });

    //     s.sc = Some(s.install_fold().expect("could not install fold function"));
    //     s.send_pattern();
    //     s
    // }

    fn delay_control(&mut self, rtt: u32, actual_acked: u32, now: u64) {
        let increase = rtt as u64 * 1460u64 > (((rtt - self.rtt_win.get_min_rtt()) as f64) * self.delta_manager.get_delta() as f64 * self.cwnd as f64) as u64;

        let mut acked = actual_acked;
        // Just in case. Sometimes CCP returns after significantly longer than
        // what was asked for. In that case, actual_acked can be huge
        if actual_acked > self.cwnd {
	          acked = self.cwnd;
        }
        // Update velocity
        if increase {
            self.cur_direction += 1;
        }
        else {
            self.cur_direction -= 1;
        }
        // TODO(venkatar): Time (now) may be a u32 internally, which means it
        // will wrap around. Handle this.

        if now - self.prev_update_rtt >= rtt as u64 && !self.slow_start {
            if (self.prev_direction > 0 && self.cur_direction > 0)
                || (self.prev_direction < 0 && self.cur_direction < 0) {
                self.velocity *= 2;
            }
            else {
                self.velocity = 1;
            }
            if self.velocity > 0xffff {
                self.velocity = 0xffff;
            }
            self.prev_direction = self.cur_direction;
            self.cur_direction = 0;
           self.prev_update_rtt = now;
        }

        // Change window
        if self.slow_start {
            if increase {
                self.cwnd += acked;
            }
            else {
                self.slow_start = false;
            }
        }
        else {
            let mut velocity = 1u64;
            if (increase && self.prev_direction > 0) ||
                (!increase && self.prev_direction < 0) {
                velocity = self.velocity as u64;
            }
            // Do computations in u64 to avoid overflow. Multiply first so
            // integer division doesn't cause as many problems
            let change = (velocity * 1448 * (acked as u64) / (self.cwnd as f32 * self.delta_manager.get_delta()) as u64) as u32;

            if increase {
                self.cwnd += change;
            }
            else {
                if change + self.init_cwnd > self.cwnd {
                    self.cwnd = self.init_cwnd;
                    self.velocity = 1;
                }
                else {
                    self.cwnd -= change;
                }
            }
        }
    }

    fn handle_timeout(&mut self) {
        // self.ss_thresh /= 2;
        // if self.ss_thresh < self.init_cwnd {
        //     self.ss_thresh = self.init_cwnd;
        // }

        // self.cwnd = self.init_cwnd;
        // self.curr_cwnd_reduction = 0;

        // self.logger.as_ref().map(|log| {
        //     warn!(log, "timeout"; 
        //         "curr_cwnd (pkts)" => self.cwnd / 1448, 
        //         "ssthresh" => self.ss_thresh,
        //     );
        // });

        // self.send_pattern();
        // return;
    }
}

impl<T: Ipc> CongAlg<T> for Copa<T> {
    type Config = CopaConfig;

    fn name() -> String {
        String::from("copa")
    }

    fn create(control: Datapath<T>, cfg: Config<T, Copa<T>>, info: DatapathInfo) -> Self {
        let mut s = Self {
            control_channel: control,
            logger: cfg.logger,
            cwnd: info.init_cwnd,
            init_cwnd: info.init_cwnd,
            sc: None,
            delta_manager: DeltaManager::new(cfg.config.default_delta, cfg.config.delta_mode),
            sock_id: info.sock_id,
            slow_start: true,
            rtt_win: RTTWindow::new(),
            velocity: 1,
            cur_direction: 0,
            prev_direction: 0,
            prev_update_rtt: 0,
            agg_measurement: AggMeasurement::new(0.5),
        };

        if cfg.config.init_cwnd != 0 {
            s.cwnd = cfg.config.init_cwnd;
            s.init_cwnd = cfg.config.init_cwnd;
        }

        s.logger.as_ref().map(|log| {
            info!(log, "starting copa flow"; "sock_id" => info.sock_id);
        });

        s.sc = Some(s.install_fold().expect("could not install fold function"));
        s.send_pattern();
        s
    }

    fn measurement(&mut self, _sock_id: u32, m: Measurement) {
        let (report_status, was_timeout, acked, _sacked, loss, inflight, rtt,
             min_rtt, now) = self.agg_measurement.report(m, &self.sc);
        if report_status == ReportStatus::UrgentReport {
            if was_timeout {
                self.handle_timeout();
            }
            self.delta_manager.report_measurement(&mut self.rtt_win,
                                                  0, loss, now);
        }
        else if report_status == ReportStatus::NoReport ||
            acked + loss + _sacked == 0{
                // Do nothing
            }
        else {
            // Record RTT
            self.rtt_win.new_rtt_sample(min_rtt, now);
            // Update delta mode and delta
            self.delta_manager.report_measurement(&mut self.rtt_win, acked, loss, now);

            // Increase/decrease the cwnd corresponding to new measurements
            self.delay_control(min_rtt, acked, now);
        }

        // Send decisions to CCP
        self.send_pattern();

        let tcp_mode = match self.delta_manager.get_mode() {
            DeltaMode::Default => "const",
            DeltaMode::TCPCoop => "tcp",
            DeltaMode::Loss => "loss",
        };
        self.logger.as_ref().map(|log| {
            debug!(log, "got ack";
                   "acked(pkts)" => acked / 1448u32,
                   "curr_cwnd (pkts)" => self.cwnd / 1460,
                   "inflight (pkts)" => inflight,
                   "loss" => loss,
                   "delta" => self.delta_manager.get_delta(),
                   "rtt" => rtt,
                   "min_rtt" => self.rtt_win.get_min_rtt(),
                   "velocity" => self.velocity,
                   "mode" => tcp_mode,
            );
        });
    }
}
