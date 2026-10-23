use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tether_core::{
    identity::Identity,
    link::with_default_port,
    pairing::PairingOffer,
    paths::{default_device_name, Paths},
    permissions::PermissionSet,
    session::{accept, connect, ClientMode, HostConfig, Outcome},
    trust::TrustStore,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

#[derive(Parser)]
#[command(
    name = "tether",
    version,
    about = "Step 1 of Tether: pair two devices and open an encrypted session"
)]
struct Cli {
    /// Directory for this device's identity and trust store.
    #[arg(long, global = true, env = "TETHER_HOME")]
    home: Option<PathBuf>,
    /// Device name shown to peers. Defaults to the hostname.
    #[arg(long, global = true)]
    name: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show this device's name, fingerprint and storage location.
    Id,
    /// Accept connections from paired devices.
    Listen {
        #[arg(long, default_value = "0.0.0.0:7420")]
        bind: String,
        /// Show a one-time pairing code so a new device can pair.
        #[arg(long)]
        pair: bool,
        /// How long the pairing code stays valid.
        #[arg(long, default_value_t = 300)]
        pair_ttl_secs: u64,
        /// What the device that pairs with this code may do: any of view, input, media,
        /// power, files (comma separated), or `all`. Change it later with `peers grant`.
        #[arg(long, default_value = "view")]
        grant: PermissionSet,
        /// Let devices with the view permission watch a stream. Only `test-pattern` exists so
        /// far; screen capture comes in a later step.
        #[arg(long, value_parser = ["test-pattern"])]
        video: Option<String>,
        /// Let devices that hold the input permission control this computer's mouse and
        /// keyboard. Off unless you pass this.
        #[arg(long)]
        input: bool,
    },
    /// Pair with a host that is running `listen --pair`.
    Pair {
        /// Host address, `host` or `host:port`.
        addr: String,
        #[arg(long)]
        code: String,
    },
    /// Open an authenticated session to a paired host and measure round trip time.
    Connect {
        addr: String,
        #[arg(long, default_value_t = 5)]
        pings: u32,
    },
    /// Manage paired devices.
    Peers {
        #[command(subcommand)]
        action: PeersCmd,
    },
}

#[derive(Subcommand)]
enum PeersCmd {
    List,
    /// Remove by name or fingerprint prefix.
    Remove {
        query: String,
    },
    /// Add permissions to a paired device, e.g. `peers grant laptop input,media`.
    Grant {
        /// Name or fingerprint prefix.
        query: String,
        perms: PermissionSet,
    },
    /// Take permissions away from a paired device, e.g. `peers revoke laptop power`.
    Revoke {
        query: String,
        perms: PermissionSet,
    },
}

struct Ctx {
    paths: Paths,
    identity: Identity,
    name: String,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tether=info,tether_core=info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let paths = Paths::resolve(cli.home).context("could not prepare the data directory")?;
    let identity = Identity::load_or_create(&paths.identity_file())
        .context("could not load or create this device's identity")?;
    let name = cli.name.unwrap_or_else(default_device_name);
    let ctx = Ctx {
        paths,
        identity,
        name,
    };

    match cli.cmd {
        Cmd::Id => show_id(&ctx),
        Cmd::Listen {
            bind,
            pair,
            pair_ttl_secs,
            grant,
            video,
            input,
        } => listen(ctx, bind, pair, pair_ttl_secs, grant, video, input).await,
        Cmd::Pair { addr, code } => pair_with(ctx, &addr, code).await,
        Cmd::Connect { addr, pings } => connect_to(ctx, &addr, pings).await,
        Cmd::Peers { action } => peers(ctx, action),
    }
}

fn show_id(ctx: &Ctx) -> Result<()> {
    println!("name         {}", ctx.name);
    println!("fingerprint  {}", ctx.identity.fingerprint());
    println!("data dir     {}", ctx.paths.dir.display());
    Ok(())
}

fn peers(ctx: Ctx, action: PeersCmd) -> Result<()> {
    let mut store = TrustStore::load(ctx.paths.trust_file())?;
    match action {
        PeersCmd::List => {
            if store.peers().is_empty() {
                println!("no paired devices");
            }
            for p in store.peers() {
                println!(
                    "{:<24} {:<8} {:<24} {}",
                    p.name,
                    p.platform,
                    p.permissions,
                    p.fingerprint()
                );
            }
        }
        PeersCmd::Remove { query } => {
            let n = store.remove(&query)?;
            if n == 0 {
                bail!("no paired device matches {query:?}");
            }
            println!("removed {n} device(s)");
        }
        PeersCmd::Grant { query, perms } => {
            change(&mut store, &query, |p| p.union(perms))?;
        }
        PeersCmd::Revoke { query, perms } => {
            change(&mut store, &query, |p| p.minus(perms))?;
        }
    }
    Ok(())
}

/// Applies a permission change and prints the result. A running `tether listen` picks the
/// change up on the device's next message, so no restart is needed.
fn change(
    store: &mut TrustStore,
    query: &str,
    f: impl Fn(PermissionSet) -> PermissionSet,
) -> Result<()> {
    let n = store.change_permissions(query, f)?;
    if n == 0 {
        bail!("no paired device matches {query:?}");
    }
    for p in store.peers().iter().filter(|p| p.matches(query)) {
        println!("{} now has: {}", p.name, p.permissions);
    }
    Ok(())
}

async fn listen(
    ctx: Ctx,
    bind: String,
    pair: bool,
    ttl_secs: u64,
    grant: PermissionSet,
    video: Option<String>,
    input: bool,
) -> Result<()> {
    let trust = TrustStore::load(ctx.paths.trust_file())?;
    let pairing = Arc::new(Mutex::new(None));
    println!("device       {}", ctx.name);
    println!("fingerprint  {}", ctx.identity.fingerprint());
    if pair {
        let offer = PairingOffer::new(Duration::from_secs(ttl_secs)).with_grant(grant);
        println!(
            "pairing code {}   (valid {} minutes, single use)",
            offer.display_code(),
            ttl_secs.div_ceil(60)
        );
        println!("will grant   {grant}");
        *pairing.lock().expect("pairing lock") = Some(offer);
    }
    let host = Arc::new(HostConfig {
        identity: Arc::new(ctx.identity),
        trust: Arc::new(Mutex::new(trust)),
        pairing,
        device_name: ctx.name,
        media: video.map(|_| {
            Arc::new(tether_media::VideoHost::test_pattern(
                tether_media::MediaOptions::default(),
            )) as Arc<dyn tether_core::media::MediaProvider>
        }),
        input: input.then(|| {
            Arc::new(tether_input::EnigoInput) as Arc<dyn tether_core::input::InputProvider>
        }),
    });

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("could not bind {bind}"))?;
    println!("listening    {}", listener.local_addr()?);
    let limit = Arc::new(Semaphore::new(16));

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (stream, remote) = res?;
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    tracing::warn!(%remote, "too many connections, dropping");
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let host = host.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match accept(stream, &host).await {
                        Ok(Outcome::Authenticated(session)) => {
                            let peer = session.peer.clone();
                            tracing::info!(%remote, peer = %peer.name, fingerprint = %peer.fingerprint(), permissions = %session.permissions, "session opened");
                            match session.serve(&host).await {
                                Ok(()) => tracing::info!(peer = %peer.name, "session closed"),
                                Err(e) => tracing::warn!(peer = %peer.name, "session ended: {e}"),
                            }
                        }
                        Ok(Outcome::Paired(peer)) => {
                            println!("paired with {} ({}) {}", peer.name, peer.platform, peer.fingerprint());
                        }
                        Err(e) => tracing::warn!(%remote, "connection refused: {e}"),
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                println!("shutting down");
                return Ok(());
            }
        }
    }
}

async fn dial(addr: &str) -> Result<TcpStream> {
    let target = with_default_port(addr);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&target))
        .await
        .map_err(|_| anyhow::anyhow!("timed out connecting to {target}"))?
        .with_context(|| format!("could not connect to {target}"))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

async fn pair_with(ctx: Ctx, addr: &str, code: String) -> Result<()> {
    let mut trust = TrustStore::load(ctx.paths.trust_file())?;
    let stream = dial(addr).await?;
    match connect(
        stream,
        &ctx.identity,
        &mut trust,
        &ctx.name,
        ClientMode::Pair { code },
    )
    .await?
    {
        Outcome::Paired(peer) => {
            println!(
                "paired with {} ({}) {}",
                peer.name,
                peer.platform,
                peer.fingerprint()
            );
            Ok(())
        }
        Outcome::Authenticated(_) => bail!("unexpected session while pairing"),
    }
}

async fn connect_to(ctx: Ctx, addr: &str, pings: u32) -> Result<()> {
    let mut trust = TrustStore::load(ctx.paths.trust_file())?;
    let stream = dial(addr).await?;
    let Outcome::Authenticated(mut session) = connect(
        stream,
        &ctx.identity,
        &mut trust,
        &ctx.name,
        ClientMode::Connect,
    )
    .await?
    else {
        bail!("unexpected pairing result");
    };
    println!(
        "connected to {} ({}) {}",
        session.peer.name,
        session.peer.platform,
        session.peer.fingerprint()
    );
    println!("allowed to   {}", session.permissions);
    for i in 0..pings {
        let rtt = session.ping(u64::from(i)).await?;
        println!("ping {}: {:.2} ms", i + 1, rtt.as_secs_f64() * 1000.0);
    }
    session.bye("done").await?;
    Ok(())
}
