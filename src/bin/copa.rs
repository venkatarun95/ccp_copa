extern crate clap;

#[macro_use]
extern crate slog;
extern crate slog_term;
extern crate slog_async;
use slog::Drain;

extern crate ccp_copa;
extern crate portus;

use clap::Arg;
use ccp_copa::Copa;
use portus::ipc::{BackendBuilder, ListenMode};

fn make_logger() -> slog::Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    slog::Logger::root(drain, o!())
}

fn make_args() -> Result<(ccp_copa::CopaConfig, String), std::num::ParseIntError> {
    let matches = clap::App::new("CCP Copa")
        .version("0.1.0")
        .author("Venkat Arun <venkatar@mit.edu>")
        .about("Implementation of Copa Congestion Control")
        .arg(Arg::with_name("ipc")
             .long("ipc")
             .help("Sets the type of ipc to use: (netlink|unix)")
             .default_value("unix")
             .validator(portus::algs::ipc_valid))
        .arg(Arg::with_name("init_cwnd")
             .long("init_cwnd")
             .help("Sets the initial congestion window, in bytes. Setting 0 will use datapath default.")
             .default_value("0"))
        .arg(Arg::with_name("default_delta")
             .long("default_delta")
             .help("Delta to use when in default mode.")
             .default_value("0.5"))
        .get_matches();

    Ok((
        ccp_copa::CopaConfig {
            init_cwnd: u32::from_str_radix(matches.value_of("init_cwnd").unwrap(), 10)?,
            default_delta: (matches.value_of("default_delta").unwrap()).parse().unwrap(),
            delta_mode: ccp_copa::DeltaModeConf::Auto,
        },
        String::from(matches.value_of("ipc").unwrap()),
    ))
}

#[cfg(not(target_os = "linux"))]
fn main() {
    let log = make_logger();
    let (cfg, ipc) = make_args()
        .map_err(|e| warn!(log, "bad argument"; "err" => ?e))
        .unwrap_or(Default::default());

    info!(log, "starting CCP Copa");
    match ipc.as_str() {
        "unix" => {
            use portus::ipc::unix::Socket;
            let b = Socket::new("in", "out")
                .map(|sk| BackendBuilder {sock: sk,  mode: ListenMode::Blocking})
                .expect("ipc initialization");
            portus::run::<_, Copa<_>>(
                b,
                &portus::Config {
                    logger: Some(log),
                    config: cfg,
                }
                ).unwrap();
        }
        _ => unreachable!(),
    }
}

#[cfg(all(target_os = "linux"))]
fn main() {
    let log = make_logger();
    let (cfg, ipc) = make_args()
        .map_err(|e| warn!(log, "bad argument"; "err" => ?e))
        .unwrap_or(Default::default());

    info!(log, "starting CCP Copa");
    match ipc.as_str() {
        "unix" => {
            use portus::ipc::unix::Socket;
            let b = Socket::new("in", "out")
                .map(|sk| BackendBuilder {sock: sk,  mode: ListenMode::Blocking})
                .expect("ipc initialization");
            portus::run::<_, Copa<_>>(
                b,
                &portus::Config {
                    logger: Some(log),
                    config: cfg,
                }
                ).unwrap();
        }
        #[cfg(all(target_os = "linux"))]
        "netlink" => {
            use portus::ipc::netlink::Socket;
            let b = Socket::new()
                .map(|sk| BackendBuilder {sock: sk,  mode: ListenMode::Blocking})
                .expect("ipc initialization");
            portus::run::<_, Copa<_>>(
                b,
                &portus::Config {
                    logger: Some(log),
                    config: cfg,
                }
                ).unwrap();
        }
        _ => unreachable!(),
    }
}
