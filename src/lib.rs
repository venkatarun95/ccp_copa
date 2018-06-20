extern crate clap;

#[macro_use]
extern crate slog;
extern crate portus;

use portus::{CongAlg, Config, Datapath, DatapathInfo, DatapathTrait, Report};
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
    sc: Scope,
    delta_manager: DeltaManager,
    prev_report_time: u64,
    cwnd: u32,
    init_cwnd: u32,
    slow_start: bool,
    rtt_win: RTTWindow,
    velocity: u32,
    cur_direction: i64,
    prev_direction: i64,
    time_since_direction: u64,
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
        (2 * self.cwnd as u64 * 1_000_000 / self.rtt_win.get_base_rtt() as u64) as u32
    }

    fn update(&self) {
        self.control_channel.update_field(
            &self.sc, 
            &[("Cwnd", self.cwnd), ("Rate", self.compute_rate())],
        ).unwrap()
    }

    fn install(&self) -> Scope {
        self.control_channel.install(
            b"
                (def
                    (Report 
                        (volatile acked 0)
                        (volatile sacked 0) 
                        (volatile loss 0)
                        (volatile inflight 0)
                        (volatile timeout 0)
                        (volatile rtt 0)
                        (volatile now 0)
                        (volatile minrtt +infinity)
                   )
                    (basertt +infinity)
                )
                (when true
                    (:= Report.acked (+ Report.acked Ack.bytes_acked))
                    (:= Report.inflight Flow.packets_in_flight)
                    (:= Report.rtt Flow.rtt_sample_us)
                    (:= Report.minrtt (min Report.minrtt Flow.rtt_sample_us))
                    (:= basertt (min basertt Flow.rtt_sample_us))
                    (:= Report.sacked (+ Report.sacked Ack.packets_misordered))
                    (:= Report.loss Ack.lost_pkts_sample)
                    (:= Report.timeout Flow.was_timeout)
                    (:= Report.now Ack.now)
                    (fallthrough)
                )
                (when (|| Flow.was_timeout (> Report.loss 0))
                    (:= Micros 0)
                    (report)
                )
                (when (> Micros (/ basertt 2))
                    (:= Micros 0)
                    (report)
                )
            ", None,
        ).unwrap()
    }

    fn delay_control(&mut self, rtt: u32, actual_acked: u32, now: u64) {
        let increase = rtt as u64 * 1460u64 > (((rtt - self.rtt_win.get_base_rtt()) as f64) * self.delta_manager.get_delta() as f64 * self.cwnd as f64) as u64;

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

        if self.velocity > 1 && ((increase && self.prev_direction < 0) ||
                            (!increase && self.prev_direction > 0)) {
            self.velocity = 1;
            self.time_since_direction = now;
        }

        if now - self.prev_update_rtt >= 2*rtt as u64 && !self.slow_start {
            // TODO(venkatar): Time (now) may be a u32 internally, which means it
            // will wrap around. Handle this.
            if (self.prev_direction > 0 && self.cur_direction > 0)
                || (self.prev_direction < 0 && self.cur_direction < 0) {
                    if (now - self.time_since_direction) as u32 > 3*rtt {
                        self.velocity *= 2;
                    }
                    else {
                        assert!(self.velocity == 1);
                    }
            }
            else {
                self.velocity = 1;
                self.time_since_direction = now;
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

            // If we are in TCP mode, delta changes with time. Account for that.
            let delta = match !increase &&
                self.delta_manager.get_mode() == DeltaMode::TCPCoop {
                false => self.delta_manager.get_delta(),
                true => 1. / (1. + 1. / self.delta_manager.get_delta())
            };

            // Do computations in u64 to avoid overflow. Multiply first so
            // integer division doesn't cause as many problems
            let change = (velocity * 1448 * (acked as u64) / (self.cwnd as f32 * delta) as u64) as u32;

            if increase {
                self.cwnd += change;
            }
            else {
                if change + self.init_cwnd > self.cwnd {
                    self.cwnd = self.init_cwnd;
                    self.velocity = 1;
                    self.time_since_direction = now;
                }
                else {
                    self.cwnd -= change;
                }
            }
        }
        assert!(self.cwnd >= self.init_cwnd);
    }

    fn handle_timeout(&mut self) {
        self.cwnd = self.init_cwnd;
        self.slow_start = true;

        self.logger.as_ref().map(|log| {
            warn!(log, "timeout";
                "curr_cwnd (pkts)" => self.cwnd / 1448,
            );
        });
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
            sc: Default::default(),
            delta_manager: DeltaManager::new(cfg.config.default_delta, cfg.config.delta_mode),
            slow_start: true,
            rtt_win: RTTWindow::new(),
            velocity: 1,
            cur_direction: 0,
            prev_direction: 0,
            time_since_direction: 0,
            prev_update_rtt: 0,
            agg_measurement: AggMeasurement::new(0.5),
            prev_report_time: 0,
        };

        if cfg.config.init_cwnd != 0 {
            s.cwnd = cfg.config.init_cwnd;
            s.init_cwnd = cfg.config.init_cwnd;
        }

        s.logger.as_ref().map(|log| {
            info!(log, "starting copa flow"; "sock_id" => info.sock_id);
        });

        s.sc = s.install();
        s.update();
        s
    }

    fn on_report(&mut self, _sock_id: u32, m: Report) {
        let (
            report_status, 
            was_timeout, 
            acked, 
            sacked, 
            loss, 
            inflight, 
            rtt,
            min_rtt, 
            now,
        ) = self.agg_measurement.report(m, &self.sc);
        if report_status == ReportStatus::UrgentReport {
            if was_timeout {
                self.handle_timeout();
            }

            self.delta_manager.report_measurement(&mut self.rtt_win,
                                                  0, loss, now);
        }
        else if report_status == ReportStatus::NoReport ||
            acked + loss + sacked == 0 {
                // Do nothing
            }
        else {
            // Record RTT
            self.rtt_win.new_rtt_sample(min_rtt, now);
            if self.rtt_win.did_base_rtt_change() {
                self.control_channel.update_field(&self.sc, &[("base_rtt", self.rtt_win.get_base_rtt())]);
            }
            // Update delta mode and delta
              self.delta_manager.report_measurement(&mut self.rtt_win, acked, loss, now);

            // Increase/decrease the cwnd corresponding to new measurements
            self.delay_control(min_rtt, acked, now);
        }

        // Send decisions to CCP
        self.update();

        self.logger.as_ref().map(|log| {
            debug!(log, "got ack";
                   "acked(pkts)" => acked / 1448u32,
                   "curr_cwnd (pkts)" => self.cwnd / 1460,
                   "loss" => loss,
                   "sacked" => sacked,
                   "delta" => self.delta_manager.get_delta(),
                   "min_rtt" => min_rtt,
                   "base_rtt" => self.rtt_win.get_base_rtt(),
                   "velocity" => self.velocity,
                   "mode" => match self.delta_manager.get_mode() {
                       DeltaMode::Default => "const",
                       DeltaMode::TCPCoop => "tcp",
                       DeltaMode::Loss => "loss",
                   },
                   "report_interval" => now - self.prev_report_time,
            );
        });
        self.prev_report_time = now;
    }
}
