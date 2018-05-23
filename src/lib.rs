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
        (2 * self.cwnd as u64 * 1_000_000 / self.rtt_win.get_min_rtt() as u64) as u32
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
                    (Report.acked 0)
                    (Report.sacked 0)
                    (Report.loss 0)
                    (Report.inflight 0)
                    (Report.timeout false)
                    (Report.rtt 0)
                    (Report.now 0)
                    (Report.minrtt +infinity)
                    (Report.maxrtt 0)
                )
                (when true
                    (bind Report.acked (+ Report.acked Ack.bytes_acked))
                    (bind Report.inflight Ack.packets_in_flight)
                    (bind Report.rtt Flow.rtt_sample_us)
                    (bind Report.minrtt (min Report.minrtt Flow.rtt_sample_us))
                    (bind Report.maxrtt (max Report.maxrtt Flow.rtt_sample_us))
                    (bind Report.sacked (+ Report.sacked Ack.packets_misordered))
                    (bind Report.loss Ack.lost_pkts_sample)
                    (bind Report.timeout Flow.was_timeout)
                    (bind Report.now Ack.now)
                    (fallthrough)
                )
                (when (|| Flow.was_timeout (> Report.loss 0))
                    (report)
                    (reset)
                )
                (when (> Micros (/ Report.minrtt 2))
                    (report)
                    (reset)
                )
            ",
        ).unwrap()
    }

    fn delay_control(&mut self, min_rtt: u32, max_rtt: u32, actual_acked: u32, now: u64) {
        //let increase = rtt as u64 * 1460u64 > (((rtt - self.rtt_win.get_min_rtt()) as f64) * self.delta_manager.get_delta() as f64 * self.cwnd as f64) as u64;
        let rtt_thresh = ((min_rtt * 1460) as f32 /
                          (self.delta_manager.get_delta() * self.cwnd as f32))
            as u32 + self.rtt_win.get_min_rtt();
        let increase = min_rtt < rtt_thresh;

        let mut acked = actual_acked;
        // Just in case. Sometimes CCP returns after significantly longer than
        // what was asked for. In that case, actual_acked can be huge
        if actual_acked > self.cwnd {
	          acked = self.cwnd;
        }

        // Update velocity
        assert!(min_rtt <= max_rtt);
        if self.cur_direction == 1 && max_rtt > rtt_thresh {
            self.velocity = 1;
            self.cur_direction = 0;
        } else if self.cur_direction == -1 && min_rtt < rtt_thresh {
            self.velocity = 1;
            self.cur_direction = 0;
        }

        if now - self.prev_update_rtt >= min_rtt as u64 && !self.slow_start {
            if self.cur_direction != 0 {
                 if now > 3 * min_rtt as u64 + self.time_since_direction {
                    self.velocity *= 2;
                }
            }
            if self.velocity > 0xffff {
                self.velocity = 0xffff;
            }
            self.prev_update_rtt = now;
            if max_rtt < rtt_thresh {
                self.cur_direction = 1;
            } else if min_rtt > rtt_thresh {
                self.cur_direction = -1;
            }
            else {
                self.cur_direction = 0;
            }
        }
        if self.cur_direction == 0 && self.time_since_direction != 0 {
            self.time_since_direction = 0;
        } else if self.cur_direction != 0 && self.time_since_direction == 0 {
            self.time_since_direction = now;
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
            // Do computations in u64 to avoid overflow. Multiply first so
            // integer division doesn't cause as many problems
            let change = (self.velocity as u64 * 1448u64 * acked as u64 /
                          (self.cwnd as f32 * self.delta_manager.get_delta())
                          as u64) as u32;

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
        println!("max_rtt: {}, rtt_thresh: {}", max_rtt, rtt_thresh);
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
            sc: Default::default(),
            delta_manager: DeltaManager::new(cfg.config.default_delta, cfg.config.delta_mode),
            slow_start: true,
            rtt_win: RTTWindow::new(),
            velocity: 1,
            cur_direction: 0,
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
            max_rtt,
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
            // Update delta mode and delta
            self.delta_manager.report_measurement(&mut self.rtt_win, acked, loss, now);

            // Increase/decrease the cwnd corresponding to new measurements
            self.delay_control(min_rtt, max_rtt, acked, now);
        }

        // Send decisions to CCP
        self.update();

        self.logger.as_ref().map(|log| {
            debug!(log, "got ack";
                //"acked(pkts)" => acked / 1448u32,
                "curr_cwnd (pkts)" => self.cwnd / 1460,
                //"loss" => loss,
                "delta" => self.delta_manager.get_delta(),
                "min_rtt" => min_rtt,
                "win_min_rtt" => self.rtt_win.get_min_rtt(),
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
