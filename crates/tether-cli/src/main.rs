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
    session::{
        accept_with, connect, connect_with, BoxFuture, ClientMode, Confirmer, HostConfig, Outcome,
        PeerInfo,
    },
    sas::Sas,
    trust::TrustStore,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

mod service;

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
    /// Run `listen` in the background, started at login.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// List the displays that can be shared with `listen --video screen:<id>`.
    Displays,
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
        /// Let devices with the view permission watch a stream: `screen` (the primary
        /// display), `screen:<id>` (see `tether displays`) or `test-pattern`.
        #[arg(long)]
        video: Option<tether_media::VideoSpec>,
        /// Also stream sound with the video: `system` (what this computer is playing) or
        /// `tone` (a test tone). Needs --video, because sound rides the video connection.
        #[arg(long, requires = "video")]
        audio: Option<tether_media::AudioSpec>,
        /// Let devices that hold the input permission control this computer's mouse and
        /// keyboard. Off unless you pass this.
        #[arg(long)]
        input: bool,
        /// Let devices that hold the files permission read and replace the text on this
        /// computer's clipboard. Off unless you pass this.
        #[arg(long)]
        clipboard: bool,
        /// Act as a wake relay: `name=MAC` or `name=MAC@broadcast` (for example
        /// `desk=aa:bb:cc:dd:ee:ff@192.168.1.255`). Repeat for more computers. Devices that
        /// hold the power permission can then wake them with `tether wake`.
        #[arg(long = "wake", value_name = "NAME=MAC[@BROADCAST]")]
        wake: Vec<String>,
        /// Do not announce this device on the local network. Other devices will then need
        /// its address to connect.
        #[arg(long)]
        no_advertise: bool,
        /// Ask here, on this terminal, before letting a paired device connect. Needs a
        /// terminal, so it does not work under `tether service`.
        #[arg(long)]
        confirm: bool,
        /// Skip comparing symbols when someone pairs, and trust the pairing code alone.
        #[arg(long)]
        no_verify: bool,
    },
    /// Ask a paired host that is a wake relay to wake one of its computers.
    Wake {
        /// The relay's address, `host` or `host:port`.
        addr: String,
        /// The name the relay knows the computer by.
        target: String,
    },
    /// Copy text to or from the clipboard of a paired host that allows it.
    Clip {
        #[command(subcommand)]
        action: ClipAction,
    },
    /// Pair with a host that is running `listen --pair`.
    Pair {
        /// Host address, `host` or `host:port`.
        addr: String,
        #[arg(long)]
        code: String,
        /// Skip comparing symbols with the host, and trust the pairing code alone.
        #[arg(long)]
        no_verify: bool,
    },
    /// List Tether devices visible on the local network, and which of them you have paired.
    Discover {
        /// How long to look, in seconds.
        #[arg(long, default_value_t = 4)]
        secs: u64,
    },
    /// Connect to a paired host by name or fingerprint, finding it on the network. Works
    /// even if its address has changed.
    Quick {
        /// The host's name, or the start of its fingerprint (see `tether peers list`).
        target: String,
        #[arg(long, default_value_t = 5)]
        pings: u32,
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
enum ClipAction {
    /// Print the text on the host's clipboard. Needs the files permission and a host started
    /// with `--clipboard`.
    Get {
        /// The host's address, `host` or `host:port`.
        addr: String,
    },
    /// Put text on the host's clipboard: the words given, or standard input if there are none.
    Send {
        /// The host's address, `host` or `host:port`.
        addr: String,
        text: Vec<String>,
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

#[derive(Subcommand)]
enum ServiceAction {
    /// Set up (or update) the background host and start it.
    Install {
        #[arg(long, default_value = "0.0.0.0:7420")]
        bind: String,
        /// What to share: `screen`, `screen:<id>` or `test-pattern`.
        #[arg(long)]
        video: Option<tether_media::VideoSpec>,
        /// Also stream sound with the video: `system` or `tone`. Needs --video.
        #[arg(long, requires = "video")]
        audio: Option<tether_media::AudioSpec>,
        /// Allow remote control of this computer's mouse and keyboard.
        #[arg(long)]
        input: bool,
        /// Allow devices with the files permission to read and replace the clipboard.
        #[arg(long)]
        clipboard: bool,
        /// Write the definition but do not start it now.
        #[arg(long)]
        no_start: bool,
        /// Only print what would be installed.
        #[arg(long)]
        dry_run: bool,
        /// Wake targets, as for `listen --wake`.
        #[arg(long = "wake", value_name = "NAME=MAC[@BROADCAST]")]
        wake: Vec<String>,
    },
    /// Stop and remove it.
    Uninstall,
    /// Show whether it is running.
    Status,
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
    let home_override = cli.home.clone();
    let name_override = cli.name.clone();
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
        Cmd::Displays => show_displays(),
        Cmd::Service { action } => match action {
            ServiceAction::Install {
                bind,
                video,
                audio,
                input,
                clipboard,
                no_start,
                dry_run,
                wake,
            } => {
                let spec = service::Spec {
                    exe: std::env::current_exe().context("could not find this program")?,
                    home: home_override,
                    name: name_override,
                    bind,
                    video: video.map(|v| match v {
                        tether_media::VideoSpec::TestPattern => "test-pattern".to_owned(),
                        tether_media::VideoSpec::Screen(None) => "screen".to_owned(),
                        tether_media::VideoSpec::Screen(Some(id)) => format!("screen:{id}"),
                    }),
                    audio: audio.map(|a| match a {
                        tether_media::AudioSpec::System => "system".to_owned(),
                        tether_media::AudioSpec::Tone => "tone".to_owned(),
                    }),
                    input,
                    clipboard,
                    wake,
                };
                if dry_run {
                    service::show(&spec)
                } else {
                    service::install(&spec, !no_start)
                }
            }
            ServiceAction::Uninstall => service::uninstall(),
            ServiceAction::Status => service::status(),
        },
        Cmd::Listen {
            bind,
            pair,
            pair_ttl_secs,
            grant,
            video,
            audio,
            input,
            clipboard,
            wake,
            no_advertise,
            confirm,
            no_verify,
        } => {
            listen(
                ctx,
                bind,
                pair,
                pair_ttl_secs,
                grant,
                video,
                audio,
                input,
                clipboard,
                wake,
                !no_advertise,
                Terminal::new(confirm, !no_verify),
            )
            .await
        }
        Cmd::Discover { secs } => discover(ctx, secs).await,
        Cmd::Quick { target, pings } => quick(ctx, &target, pings).await,
        Cmd::Wake { addr, target } => wake_via(ctx, &addr, &target).await,
        Cmd::Clip { action } => clip(ctx, action).await,
        Cmd::Pair {
            addr,
            code,
            no_verify,
        } => pair_with(ctx, &addr, code, !no_verify).await,
        Cmd::Connect { addr, pings } => connect_to(ctx, &addr, pings).await,
        Cmd::Peers { action } => peers(ctx, action),
    }
}

fn show_displays() -> Result<()> {
    let displays = tether_media::list_displays().map_err(|e| anyhow::anyhow!(e))?;
    if displays.is_empty() {
        println!("no displays found");
    }
    for d in displays {
        println!(
            "{:<6} {}x{} at {},{}  {}{}",
            d.id,
            d.width,
            d.height,
            d.x,
            d.y,
            d.name,
            if d.primary { "  (primary)" } else { "" }
        );
    }
    Ok(())
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

#[allow(clippy::too_many_arguments)]
async fn listen(
    ctx: Ctx,
    bind: String,
    pair: bool,
    ttl_secs: u64,
    grant: PermissionSet,
    video: Option<tether_media::VideoSpec>,
    audio: Option<tether_media::AudioSpec>,
    input: bool,
    clipboard: bool,
    wake: Vec<String>,
    advertise: bool,
    terminal: Arc<Terminal>,
) -> Result<()> {
    let own_key = ctx.identity.public;
    let own_name = ctx.name.clone();
    let relay = if wake.is_empty() {
        None
    } else {
        let relay =
            tether_core::wake::WolRelay::from_specs(&wake).map_err(|e| anyhow::anyhow!(e))?;
        println!("can wake     {}", relay.names().join(", "));
        Some(Arc::new(relay) as Arc<dyn tether_core::wake::WakeProvider>)
    };
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
        media: video.map(|spec| {
            let mut host =
                tether_media::VideoHost::from_spec(spec, tether_media::MediaOptions::default());
            if let Some(audio) = audio {
                host = host.with_audio(audio);
            }
            Arc::new(host) as Arc<dyn tether_core::media::MediaProvider>
        }),
        input: input.then(|| {
            Arc::new(tether_input::EnigoInput) as Arc<dyn tether_core::input::InputProvider>
        }),
        wake: relay,
        clipboard: clipboard.then(|| {
            Arc::new(tether_clipboard::SystemClipboard::new())
                as Arc<dyn tether_core::clipboard::ClipboardProvider>
        }),
    });

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("could not bind {bind}"))?;
    println!("listening    {}", listener.local_addr()?);
    // Held until this function returns, which withdraws the announcement.
    let _advert = if advertise {
        match tether_core::discovery::Advertiser::start(
            &own_name,
            &own_key,
            tether_core::proto::Platform::current(),
            listener.local_addr()?.port(),
        ) {
            Ok(a) => {
                println!("visible as   {}", a.instance());
                Some(a)
            }
            Err(e) => {
                tracing::warn!("not announcing on the network: {e}");
                None
            }
        }
    } else {
        None
    };
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
                let terminal = terminal.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match accept_with(stream, &host, Some(&*terminal)).await {
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

async fn pair_with(ctx: Ctx, addr: &str, code: String, verify: bool) -> Result<()> {
    let mut trust = TrustStore::load(ctx.paths.trust_file())?;
    let stream = dial(addr).await?;
    let terminal = Terminal::new(false, verify);
    match connect_with(
        stream,
        &ctx.identity,
        &mut trust,
        &ctx.name,
        ClientMode::Pair { code },
        Some(&*terminal),
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

async fn wake_via(ctx: Ctx, addr: &str, target: &str) -> Result<()> {
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
    session.wake(target).await?;
    // The answer comes back as a notice, ahead of the reply to this ping.
    session.ping(0).await?;
    let notices = session.take_notices();
    session.bye("done").await?;
    if notices.is_empty() {
        bail!("the host did not answer");
    }
    for n in notices {
        println!("{n}");
    }
    Ok(())
}

async fn clip(ctx: Ctx, action: ClipAction) -> Result<()> {
    let (addr, send) = match &action {
        ClipAction::Get { addr } => (addr.clone(), None),
        ClipAction::Send { addr, text } if !text.is_empty() => (addr.clone(), Some(text.join(" "))),
        ClipAction::Send { addr, .. } => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                .context("could not read standard input")?;
            (addr.clone(), Some(text))
        }
    };
    let mut trust = TrustStore::load(ctx.paths.trust_file())?;
    let stream = dial(&addr).await?;
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
    let result = match send {
        Some(text) => {
            session.send_clipboard(&text).await?;
            // The answer comes back as a notice, ahead of the reply to this ping.
            session.ping(0).await?;
            let notices = session.take_notices();
            if notices.is_empty() {
                Err(anyhow::anyhow!("the host did not answer"))
            } else {
                for n in notices {
                    println!("{n}");
                }
                Ok(())
            }
        }
        None => match session.get_clipboard().await? {
            Some(text) => {
                print!("{text}");
                Ok(())
            }
            None => Err(anyhow::anyhow!(
                "{}",
                session.take_notices().join(" ").trim().to_owned()
            )),
        },
    };
    session.bye("done").await?;
    result
}

async fn connect_to(ctx: Ctx, addr: &str, pings: u32) -> Result<()> {
    let stream = dial(addr).await?;
    run_session(ctx, stream, pings, None).await
}

/// Opens a session on `stream` and pings. `expect` is the key the host must hold; a
/// different (but still trusted) host is refused.
async fn run_session(
    ctx: Ctx,
    stream: TcpStream,
    pings: u32,
    expect: Option<[u8; 32]>,
) -> Result<()> {
    let mut trust = TrustStore::load(ctx.paths.trust_file())?;
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
    if expect.is_some_and(|k| k != session.peer.public_key) {
        let _ = session.bye("wrong device").await;
        bail!("the device at that address is not the one you asked for");
    }
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

async fn discover(ctx: Ctx, secs: u64) -> Result<()> {
    let trust = TrustStore::load(ctx.paths.trust_file())?;
    let found = tether_core::discovery::scan(
        Duration::from_secs(secs.clamp(1, 60)),
        Some(ctx.identity.public),
    )
    .await?;
    if found.is_empty() {
        println!("no Tether devices found (is the other one running `tether listen`?)");
    }
    for d in found {
        let state = match tether_core::discovery::identify(&d, &trust) {
            Some(_) => "paired",
            None => "not paired",
        };
        let note = if d.compatible() { "" } else { "  (different protocol version)" };
        println!("{}  {}  {}  {state}{note}", d.name, d.platform, d.fingerprint);
        for a in &d.addresses {
            println!("    {a}");
        }
    }
    Ok(())
}

async fn quick(ctx: Ctx, target: &str, pings: u32) -> Result<()> {
    let trust = TrustStore::load(ctx.paths.trust_file())?;
    let mut matches = trust.peers().iter().filter(|p| p.matches(target));
    let Some(peer) = matches.next() else {
        bail!("no paired device matches {target:?} (see `tether peers list`)");
    };
    if matches.next().is_some() {
        bail!("more than one paired device matches {target:?}; use more of its fingerprint");
    }
    let fingerprint = peer.fingerprint();
    let key: [u8; 32] = hex::decode(&peer.public_key)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("the stored key for {} is damaged", peer.name))?;
    let name = peer.name.clone();
    println!("looking for {name} on the network...");
    let device = tether_core::discovery::find(
        &fingerprint,
        Duration::from_secs(6),
        Some(ctx.identity.public),
    )
    .await?
    .with_context(|| format!("{name} did not answer; is it running `tether listen`?"))?;
    let (stream, addr) = tether_core::discovery::dial(&device, Duration::from_secs(4))
        .await
        .with_context(|| format!("found {name} but could not connect"))?;
    println!("found at     {addr}");
    run_session(ctx, stream, pings, Some(key)).await
}

/// Asks the person at this terminal. One question at a time, so prompts from two devices
/// connecting together do not interleave.
struct Terminal {
    /// Ask before letting a paired device connect.
    approve: bool,
    /// Ask people to compare symbols when pairing.
    verify: bool,
    one_at_a_time: tokio::sync::Mutex<()>,
}

impl Terminal {
    fn new(approve: bool, verify: bool) -> Arc<Self> {
        Arc::new(Self {
            approve,
            verify,
            one_at_a_time: tokio::sync::Mutex::new(()),
        })
    }
}

/// True only for `y` or `yes`. End of input (no terminal) counts as no.
async fn ask_yes_no(question: String) -> bool {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        print!("{question}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(n) if n > 0 => matches!(line.trim().to_lowercase().as_str(), "y" | "yes"),
            _ => false,
        }
    })
    .await
    .unwrap_or(false)
}

impl Confirmer for Terminal {
    fn approve_connection<'a>(&'a self, peer: &'a PeerInfo) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            if !self.approve {
                return true;
            }
            let _turn = self.one_at_a_time.lock().await;
            ask_yes_no(format!(
                "\n{} ({}) {} wants to connect. Allow? [y/N] ",
                peer.name,
                peer.platform,
                peer.fingerprint()
            ))
            .await
        })
    }

    fn verify_sas<'a>(&'a self, sas: &'a Sas, peer: &'a PeerInfo) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            if !self.verify {
                return true;
            }
            let _turn = self.one_at_a_time.lock().await;
            let emoji = sas.emoji();
            println!("\nCompare with {} ({}):", peer.name, peer.platform);
            println!("  {}", emoji.map(|(s, _)| s).join("  "));
            println!("  {}", emoji.map(|(_, n)| n).join(", "));
            println!("  or as numbers: {}", sas.digits());
            ask_yes_no("Does the other device show the same? [y/N] ".to_owned()).await
        })
    }
}
