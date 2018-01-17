extern crate clap;

#[macro_use]
extern crate slog;
#[macro_use]
extern crate portus;

use portus::{CongAlg, Config, Datapath, DatapathInfo, Measurement};
use portus::pattern;
use portus::ipc::Ipc;
use portus::lang::Scope;

pub struct Copa<T: Ipc> {
    control_channel: Datapath<T>,
    logger: Option<slog::Logger>,
    sc: Option<Scope>,
    sock_id: u32,
    cwnd: u32,
    curr_cwnd_reduction: u32,
    init_cwnd: u32,
    slow_start: bool,
    delta: f32,
    min_rtt: u32,
}

#[derive(Clone)]
pub enum DeltaMode {Constant, Auto}

#[derive(Clone)]
pub struct CopaConfig {
    pub init_cwnd: u32,
    pub default_delta: f32,
    pub delta_mode: DeltaMode,
}

impl Default for CopaConfig {
    fn default() -> Self {
        CopaConfig {
            init_cwnd: 0,
            default_delta: 0.5,
            delta_mode: DeltaMode::Auto,
        }
    }
}

impl<T: Ipc> Copa<T> {
    fn send_pattern(&self) {
        match self.control_channel.send_pattern(
            self.sock_id,
            make_pattern!(
                pattern::Event::SetCwndAbs(self.cwnd) =>
                pattern::Event::WaitRtts(1.0) => 
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
        let mut min_rtt = self.min_rtt;
        if self.min_rtt == std::u32::MAX {
            // Fold functions onlys support 30-bit values
            min_rtt = 0x3fffffff;
        }
        print!("
                (def (acked 0) (sacked 0) (loss 0) (timeout false) (rtt 0) (inflight 0) (minrtt +infinity) (overcnt 0) (undercnt 0))
                (bind Flow.inflight Pkt.packets_in_flight)
                (bind Flow.rtt Pkt.rtt_sample_us)
                (bind Flow.minrtt (min Flow.minrtt Pkt.rtt_sample_us))
                (bind Flow.acked (+ Flow.acked Pkt.bytes_acked))
                (bind Flow.sacked (+ Flow.sacked Pkt.packets_misordered))
                (bind Flow.loss Pkt.lost_pkts_sample)
                (bind Flow.timeout Pkt.was_timeout)
                (bind isUrgent Pkt.was_timeout)
                (bind isUrgent (!if isUrgent (> Flow.loss 0)))
                (bind minrtt {})
                (bind threshold (/ (- Pkt.rtt_sample_us minrtt) {}))
                (bind threshold (if (== minrtt 0) (+ 0 0)))
                (bind increase (> (div (* 1460 Pkt.rtt_sample_us) Cwnd) threshold))
                (bind Flow.overcnt (if increase (+ Flow.overcnt 1)))
                (bind Flow.undercnt (!if increase (+ Flow.undercnt 1)))
            ", min_rtt, (1. / self.delta) as u32);
        //  (* {} (- Pkt.rtt_sample_us {})))
        // (> (div (* 1460 Pkt.rtt_sample_us) Cwnd) threshold))
        // (if increase (+ Flow.over_cnt 1))
        // (!if increase (+ Flow.under_cnt 1))
        Some(self.control_channel.install_measurement(
            self.sock_id,
            format!("
                (def (acked 0) (sacked 0) (loss 0) (timeout false) (rtt 0) (minrtt +infinity) (overcnt 0) (undercnt 0))
                (bind Flow.rtt Pkt.rtt_sample_us)
                (bind Flow.minrtt (min Flow.minrtt Pkt.rtt_sample_us))
                (bind Flow.minrtt (min Flow.minrtt {}))
                (bind Flow.acked (+ Flow.acked Pkt.bytes_acked))
                (bind Flow.sacked (+ Flow.sacked Pkt.packets_misordered))
                (bind Flow.loss Pkt.lost_pkts_sample)
                (bind Flow.timeout Pkt.was_timeout)
                (bind isUrgent Pkt.was_timeout)
                (bind isUrgent (!if isUrgent (> Flow.loss 0)))
                (bind threshold (/ (- Pkt.rtt_sample_us Flow.minrtt) {}))
                (bind increase (> (div (* 1460 Pkt.rtt_sample_us) Cwnd) threshold))
                (bind Flow.overcnt (if increase (+ Flow.overcnt 1)))
                (bind Flow.undercnt (!if increase (+ Flow.undercnt 1)))
            ", min_rtt, (1. / self.delta) as u32)
                .as_bytes(),
        ).unwrap())
    }

    fn get_fields(&mut self, m: Measurement) -> (u32, bool, u32, u32, u32, u32, u32, u32, u32) {
        let sc = self.sc.as_ref().expect("scope should be initialized");
        let sack = m.get_field(&String::from("Flow.sacked"), sc).expect(
            "expected sacked field in returned measurement",
        ) as u32;

        let ack = m.get_field(&String::from("Flow.acked"), sc).expect(
            "expected acked field in returned measurement",
        ) as u32;

        let was_timeout = m.get_field(&String::from("Flow.timeout"), sc).expect(
            "expected timeout field in returned measurement",
        ) as u32;

        let inflight = m.get_field(&String::from("Flow.inflight"), sc).expect(
            "expected inflight field in returned measurement",
        ) as u32;

        let loss = m.get_field(&String::from("Flow.loss"), sc).expect(
            "expected loss field in returned measurement",
        ) as u32;

        let rtt = m.get_field(&String::from("Flow.rtt"), sc).expect(
            "expected rtt field in returned measurement",
        ) as u32;

        let min_rtt = m.get_field(&String::from("Flow.minrtt"), sc).expect(
            "expected minrtt field in returned measurement",
        ) as u32;

        let over_cnt = m.get_field(&String::from("Flow.overcnt"), sc).expect(
            "expected overcnt field in returned measurement",
        ) as u32;

        let under_cnt = m.get_field(&String::from("Flow.undercnt"), sc).expect(
            "expected undercnt field in returned measurement",
        ) as u32;

        (ack, was_timeout == 1, sack, loss, rtt, inflight, min_rtt, over_cnt, under_cnt)
    }

    fn delay_control(&mut self, over_cnt: u32, under_cnt: u32, acked: u32) {
        // // Equation rearranged to avoid integer division
        // let increase = rtt as u64 * 1460u64 > (((rtt - self.min_rtt) as f64) * self.delta as f64 * self.cwnd as f64) as u64;
        let increase = over_cnt > under_cnt;
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
            let change = (1448 * (acked as u64) / (self.cwnd as u64)) as u32;
            if increase {
                self.cwnd += change;
            }
            else {
                if change + self.init_cwnd > self.cwnd {
                    self.cwnd = self.init_cwnd;
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

    /// Handle sacked or lost packets
    /// Only call with loss > 0 || sacked > 0
    fn cwnd_reduction(&mut self, loss: u32, sacked: u32, acked: u32) {
        // if loss indicator is nonzero
        // AND the losses in the lossy cwnd have not yet been accounted for
        // OR there is a partial ACK AND cwnd was probing ss_thresh

        // if loss > 0 && self.curr_cwnd_reduction == 0 || (acked > 0 && self.cwnd == self.ss_thresh) {
        //     self.cwnd /= 2;
        //     if self.cwnd <= self.init_cwnd {
        //         self.cwnd = self.init_cwnd;
        //     }

        //     self.ss_thresh = self.cwnd;
        //     self.send_pattern();
        // }

        // self.curr_cwnd_reduction += sacked + loss;
        // self.logger.as_ref().map(|log| {
        //     info!(log, "loss"; "curr_cwnd (pkts)" => self.cwnd / 1448, "loss" => loss, "sacked" => sacked, "curr_cwnd_deficit" => self.curr_cwnd_reduction);
        // });
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
            curr_cwnd_reduction: 0,
            sc: None,
            sock_id: info.sock_id,
            slow_start: true,
            delta: cfg.config.default_delta,
            min_rtt: std::u32::MAX,
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
        let (acked, was_timeout, sacked, loss, rtt, inflight, min_rtt, over_cnt, under_cnt) = self.get_fields(m);
        if was_timeout {
            self.handle_timeout();
            return;
        }
        if self.min_rtt > min_rtt {
            self.min_rtt = min_rtt;
        }

        // increase the cwnd corresponding to new in-order cumulative ACKs
        self.delay_control(over_cnt, under_cnt, acked);

        if loss > 0 || sacked > 0 {
            self.cwnd_reduction(loss, sacked, acked);
        } else if acked < self.curr_cwnd_reduction {
            self.curr_cwnd_reduction -= acked / 1448u32;
        } else {
            self.curr_cwnd_reduction = 0;
        }

        if self.curr_cwnd_reduction > 0 {
            self.logger.as_ref().map(|log| {
                debug!(log, "in cwnd reduction"; "acked" => acked / 1448u32, "deficit" => self.curr_cwnd_reduction);
            });
            return;
        }

        self.send_pattern();
        self.sc = Some(self.install_fold().expect("could not install fold function"));

        self.logger.as_ref().map(|log| {
            debug!(log, "got ack";
                "acked(pkts)" => acked / 1448u32,
                "curr_cwnd (pkts)" => self.cwnd / 1460,
                "inflight (pkts)" => inflight,
                "loss" => loss,
                "delta" => self.delta,
                "rtt" => rtt,
                "min_rtt" => self.min_rtt,
            );
        });
    }
}
