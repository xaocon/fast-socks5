#![forbid(unsafe_code)]
#[macro_use]
extern crate log;

use fast_socks5::{
    client::{self, Socks5Stream},
    server::{Config, SimpleUserPassword, Socks5Server, Socks5Socket},
    Result, SocksError,
};
use std::{io::ErrorKind, net::ToSocketAddrs};
use std::future::Future;
use structopt::StructOpt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task;
use tokio_stream::StreamExt;
use anyhow::{anyhow, Context};

const UPSTREAM_PROXY: &str = "127.0.0.1:1333";

/// # How to use it:
///
/// Listen on a local address, authentication-free:
///     `$ RUST_LOG=debug cargo run --example server -- --listen-addr 127.0.0.1:1337 no-auth`
///
/// Listen on a local address, with basic username/password requirement:
///     `$ RUST_LOG=debug cargo run --example server -- --listen-addr 127.0.0.1:1337 password --username admin --password password`
///
#[derive(Debug, StructOpt)]
#[structopt(
    name = "socks5-server",
    about = "A simple implementation of a socks5-server."
)]
struct Opt {
    /// Bind on address address. eg. `127.0.0.1:1080`
    #[structopt(short, long)]
    pub listen_addr: String,

    /// Request timeout
    #[structopt(short = "t", long, default_value = "1000")]
    pub request_timeout: u64,

    /// Don't perform the auth handshake, send directly the command request
    #[structopt(short = "k", long)]
    pub skip_auth: bool,
}

/// Useful read 1. https://blog.yoshuawuyts.com/rust-streams/
/// Useful read 2. https://blog.yoshuawuyts.com/futures-concurrency/
/// Useful read 3. https://blog.yoshuawuyts.com/streams-concurrency/
/// error-libs benchmark: https://blog.yoshuawuyts.com/error-handling-survey/
///
/// TODO: Write functional tests: https://github.com/ark0f/async-socks5/blob/master/src/lib.rs#L762
/// TODO: Write functional tests with cURL?
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();

    spawn_socks_server().await
}

async fn spawn_socks_server() -> Result<()> {
    let opt: Opt = Opt::from_args();
    let config = Config::default()
        .set_request_timeout(opt.request_timeout)
        .set_skip_auth(opt.skip_auth)
        .set_dns_resolve(false);

    // let config = match opt.auth {
    //     AuthMode::NoAuth => {
    //         warn!("No authentication has been set!");
    //         config
    //     }
    //     AuthMode::Password { username, password } => {
    //         if opt.skip_auth {
    //             return Err(SocksError::ArgumentInputError(
    //                 "Can't use skip-auth flag and authentication altogether.",
    //             ));
    //         }

    //         info!("Simple auth system has been set.");
    //         config.with_authentication(SimpleUserPassword { username, password })
    //     }
    // };

    let listener = <Socks5Server>::bind(&opt.listen_addr).await?;
    let listener = listener.with_config(config);

    let mut incoming = listener.incoming();

    info!("Listen for socks connections @ {}", &opt.listen_addr);

    // Standard TCP loop
    while let Some(socket_res) = incoming.next().await {
        match socket_res {
            Ok(socket) => {
                spawn_and_log_error(socket.upgrade_to_socks5());
            }
            Err(err) => {
                error!("accept error = {:?}", err);
            }
        }
    }

    Ok(())
}

fn spawn_and_log_error<F, T>(fut: F) -> task::JoinHandle<()>
where
    F: Future<Output = Result<Socks5Socket<T, SimpleUserPassword>>> + Send + 'static,
    T: AsyncRead + AsyncWrite + Unpin + Send, // Add the Send trait here
{
    task::spawn(async move {
        match fut.await {
            Ok(mut socks5_socket) => {
                trace!("It's here");

                // resolve dns
                socks5_socket
                    .resolve_dns()
                    .await.unwrap();

                // get actual socket address
                let target_addr = socks5_socket
                    .target_addr().unwrap();
                debug!(
                    "incoming request resolved target address to: {}",
                    target_addr
                );

                let socket_addr = target_addr
                    .to_socket_addrs()
                    .unwrap()
                    .next()
                    .unwrap();

                // connect to downstream proxy
                let mut stream = Socks5Stream::connect(
                    UPSTREAM_PROXY,
                    socket_addr.ip().to_string(),
                    socket_addr.port(),
                    client::Config::default(),
                )
                .await.unwrap();

                match tokio::io::copy_bidirectional(&mut stream, &mut socks5_socket).await {
                    Ok(res) => info!("socket transfer closed ({}, {})", res.0, res.1),
                    Err(err) => match err.kind() {
                        ErrorKind::NotConnected => {
                            info!("socket transfer closed by client");
                        }
                        ErrorKind::ConnectionReset => {
                            info!("socket transfer closed by downstream proxy");
                        }
                        _ => error!("transfer error: {:?}", err),
                    },
                };
        
            }
            Err(err) => error!("{:#}", &err),
        }
    })
}
