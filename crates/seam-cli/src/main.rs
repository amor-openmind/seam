//! `seam` — the daemon and command-line interface.
//!
//! Deliberately small. Every command that could ask the user a question instead works it
//! out: the identity is generated, the name is the hostname, the port is chosen by the
//! OS, and peers are found by discovery. The only thing a human is ever asked is to
//! confirm a 6-digit pairing code, because that confirmation *is* the security boundary
//! and cannot be automated away.

/// Focus lives on the sending side, which today means macOS. A receiving machine has no
/// layout to reason about: it acts on whatever the owner sends it.
#[cfg(target_os = "macos")]
mod focus;
mod licence;
mod layout;
mod store;

/// The target triple this binary was compiled for, so a cross-compiled build identifies
/// itself precisely.
const BUILD_TARGET: &str = env!("SEAM_BUILD_TARGET");

use std::io::{BufRead as _, Write as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use seam_transport::{
    DiscoveredPeer, Discovery, DiscoveryEvent, Endpoint, Error as TransportError, Identity, Link,
};

#[derive(Parser, Debug)]
#[command(
    name = "seam",
    version,
    about = "Share one mouse, keyboard and clipboard across machines on your network"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check this machine's setup and report anything that would stop seam working.
    Doctor,
    /// Watch for other seam machines on the network.
    Discover {
        /// How long to watch, in seconds.
        #[arg(long, default_value_t = 5)]
        r#for: u64,
    },
    /// Pair with another machine. With no name, waits for the other machine to start.
    Pair {
        /// The peer to dial, by name. Omit to wait for an incoming request instead.
        peer: Option<String>,
        /// Dial this address directly, e.g. `192.0.2.10:51820`, skipping discovery.
        ///
        /// Discovery needs to *receive* multicast, which a restrictive inbound firewall
        /// blocks. Dialling out is almost never blocked, so this always works — and it is
        /// the documented last tier of discovery, not a workaround.
        #[arg(long, value_name = "HOST:PORT")]
        at: Option<String>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// Give this machine its mouse and keyboard back.
    ///
    /// For when seam, or anything else, exited while input was handed to another machine
    /// and left this one unable to respond.
    Release,
    /// List the machines this one has paired with.
    Peers,
    /// Remove a pairing.
    Forget { peer: String },
    /// Run the daemon: advertise this machine and keep links to paired peers.
    /// Open the live fleet page served by the running daemon.
    Ui,

    /// Enter the licence that lets this machine run seam.
    Activate {
        /// The licence issued to you, beginning `seam-`.
        key: String,
    },

    Run {
        /// Port to listen on. The default is stable on purpose: a machine that picks a
        /// random port each start cannot be dialled by a peer that remembers the old one.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Windows: stay non-elevated instead of prompting UAC once at start. Input
        /// injection then dies whenever an elevated window has focus (UIPI).
        #[arg(long, default_value_t = false)]
        no_elevate: bool,
        /// Do not open the fleet page in a browser after starting — for headless or
        /// scripted runs. By default, launching seam shows you seam.
        #[arg(long, default_value_t = false)]
        no_ui: bool,
        /// Also dial these addresses. Needed where an inbound firewall blocks discovery:
        /// dialling out is not blocked, so the link still forms.
        #[arg(long = "connect", value_name = "HOST:PORT")]
        connect: Vec<String>,
    },
}

/// Where the daemon always writes its log.
///
/// Logging only to stdout means every lockup destroys its own evidence: the terminal is
/// closed, or the process is killed, and there is nothing left to diagnose from. Three
/// separate faults in this project were debugged by guesswork for exactly that reason.
fn log_path() -> Option<std::path::PathBuf> {
    let dir = store::state_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("seam.log"))
}

/// Writes to two sinks at once.
struct Tee<A: std::io::Write, B: std::io::Write>(A, B);

impl<A: std::io::Write, B: std::io::Write> std::io::Write for Tee<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // The file is best-effort: a full disk must never stop the daemon logging to the
        // terminal, and must certainly never stop it forwarding input.
        let _ = self.1.write_all(buf);
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.1.flush();
        self.0.flush()
    }
}

fn main() -> Result<()> {
    // Truncate rather than append: a stale log from a previous run is worse than none,
    // because it invites diagnosing the wrong session.
    let log_file = log_path().and_then(|path| {
        let file = std::fs::File::create(&path).ok()?;
        eprintln!("logging to {}", path.display());
        Some(file)
    });

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SEAM_LOG")
                .unwrap_or_else(|_| "seam=info,warn".into()),
        )
        .with_target(false)
        .with_writer(move || -> Box<dyn std::io::Write> {
            match &log_file {
                Some(file) => match file.try_clone() {
                    // Everything goes to both, so the terminal stays useful and the
                    // evidence survives the terminal.
                    Ok(clone) => Box::new(Tee(std::io::stdout(), clone)),
                    Err(_) => Box::new(std::io::stdout()),
                },
                None => Box::new(std::io::stdout()),
            }
        })
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(run(Cli::parse()))
}

async fn run(cli: Cli) -> Result<()> {
    let dir = store::state_dir()?;
    let identity = Arc::new(store::load_or_create_identity(&dir)?);

    // No arguments is the double-click case: run, and show the fleet page.
    let Some(command) = cli.command else {
        return run_daemon(&dir, identity, DEFAULT_PORT, Vec::new(), false, true).await;
    };
    match command {
        Command::Doctor => doctor(&dir, &identity),
        Command::Discover { r#for } => {
            discover(Duration::from_secs(r#for), identity.peer_id()).await
        }
        Command::Pair { peer, at, timeout } => {
            pair(&dir, identity, peer, at, Duration::from_secs(timeout)).await
        }
        Command::Release => {
            seam_input::release_input();
            println!("This machine's mouse and keyboard are active again.");
            Ok(())
        }
        Command::Peers => {
            peers(&dir);
            Ok(())
        }
        Command::Forget { peer } => forget(&dir, &peer),
        Command::Ui => open_ui(&dir),
        Command::Activate { key } => {
            let licence = licence::activate(&dir, &key)?;
            println!("  activated for {}", licence.name);
            if licence.expires_day == 0 {
                println!("  no expiry");
            }
            println!("\n  seam is ready. Run it with no arguments.");
            Ok(())
        }
        Command::Run { port, connect, no_elevate, no_ui } => {
            run_daemon(&dir, identity, port, connect, no_elevate, !no_ui).await
        }
    }
}

/// Run the daemon — the body of `seam run`, and of `seam` with no arguments at all
/// (a double-click on the downloaded binary), which also opens the fleet page once up.
async fn run_daemon(
    dir: &std::path::Path,
    identity: Arc<Identity>,
    port: u16,
    connect: Vec<String>,
    no_elevate: bool,
    open_ui_when_ready: bool,
) -> Result<()> {
    {
            // The licence gate — but it must not be a dead end.
            //
            // This used to exit before anything started, which meant the activation
            // screen had no server to be served from: seam refused to run, and the only
            // way in was a command line. Now an unlicensed seam starts its page and
            // nothing else. No input is captured, no port is opened to the network, no
            // peer is contacted — the machine is untouched — but activation happens where
            // a person can see it.
            if licence::stored(dir).is_none() {
                tracing::info!("no licence on this machine; opening the activation page");
                // Returns once a licence is accepted, and the normal start continues
                // below — activating should start seam, not tell you to start it again.
                serve_activation_only(dir, open_ui_when_ready).await?;
            }

            #[cfg(target_os = "windows")]
            if !no_elevate && !seam_input::windows::is_elevated() {
                // UIPI is the reason: Windows silently discards injected input while an
                // elevated window - an admin PowerShell, Task Manager - has focus, so a
                // non-elevated seam goes dead exactly when those windows are used. One
                // UAC prompt at startup buys injection that works everywhere but the
                // secure desktop.
                tracing::info!(
                    "requesting administrator rights so input keeps working when an \
                     elevated window has focus (--no-elevate to skip)"
                );
                // Build the argument list explicitly. Passing raw argv was wrong and
                // silently fatal: a bare `seam.exe` has no subcommand, so the relaunch
                // became `seam.exe --no-elevate`, which is not valid (the flag belongs to
                // `run`). The elevated copy then died on an argument error in a console
                // window that closes instantly, and the parent had already exited — from
                // the user's chair, the program printed one line and quit.
                let mut args: Vec<String> = vec!["run".into(), "--no-elevate".into()];
                if port != 0 {
                    args.push("--port".into());
                    args.push(port.to_string());
                }
                for address in &connect {
                    args.push("--connect".into());
                    args.push(address.clone());
                }
                if !open_ui_when_ready {
                    args.push("--no-ui".into());
                }
                match seam_input::windows::relaunch_elevated(&args) {
                    // Do NOT exit on the strength of a successful launch call. A
                    // successful ShellExecute means Windows accepted the request, not
                    // that a daemon is running: the child can die on a bad argument, a
                    // missing licence or a refused prompt, and the parent leaving anyway
                    // means the machine keeps whatever was running before. That is
                    // exactly how an updated binary reported the OLD version - the new
                    // one asked for elevation, vanished, and the previous daemon carried
                    // on serving its page.
                    Ok(true) => {
                        if elevated_child_started(dir) {
                            return Ok(());
                        }
                        tracing::warn!(
                            "the elevated copy did not start; carrying on without \
                             administrator rights. Input will freeze whenever an elevated \
                             window has focus."
                        );
                    }
                    Ok(false) => tracing::warn!(
                        "running without administrator rights: the pointer and keyboard \
                         will freeze whenever an elevated window has focus"
                    ),
                    Err(e) => tracing::warn!("could not relaunch elevated: {e}"),
                }
            }
            #[cfg(not(target_os = "windows"))]
            let _ = no_elevate;
            if open_ui_when_ready {
                let dir = dir.to_path_buf();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    if let Err(e) = open_ui(&dir) {
                        tracing::warn!("could not open the fleet page: {e}");
                    }
                });
            }
            daemon(dir, identity, port, connect).await
    }
}

// ---------------------------------------------------------------- doctor

fn doctor(dir: &std::path::Path, identity: &Identity) -> Result<()> {
    println!("seam doctor\n");

    println!("  this machine");
    // Version first: the commonest support question is "which build is this?", and an
    // old binary silently behaving like a new one wastes more time than any other bug.
    println!("    seam         v{} ({})", env!("CARGO_PKG_VERSION"), BUILD_TARGET);
    println!("    name         {}", Discovery::default_display_name(identity.peer_id()));
    println!("    id           {}", identity.peer_id());
    println!("    fingerprint  {}", identity.fingerprint().to_grouped_hex());
    println!("    state        {}", dir.display());
    println!("    protocol     v{}", seam_proto::PROTOCOL_VERSION);
    println!("    platform     {} / {}", std::env::consts::OS, std::env::consts::ARCH);

    let layout = layout::detect();
    println!("    keyboard     {}", layout.display());
    if matches!(layout, layout::Layout::Known { .. }) {
        println!(
            "                 composes text with {}",
            if layout::Layout::composes_with_option() { "Option" } else { "AltGr" }
        );
    }

    println!("\n  displays");
    match seam_input::desktop() {
        Ok(desktop) => {
            for d in &desktop.displays {
                let (w, h) = (d.pixels.width, d.pixels.height);
                let scale = f64::from(d.scale) / 256.0;
                let mm_w = d.width_mm / seam_input::MM;
                let mm_h = d.height_mm / seam_input::MM;
                println!(
                    "    {}{}x{} at ({},{})  {scale:.2}x  {mm_w}x{mm_h} mm",
                    if d.primary { "* " } else { "  " },
                    w,
                    h,
                    d.pixels.x,
                    d.pixels.y
                );
            }
            let bb = desktop.bounding_box();
            println!("    desktop  {}x{} at ({},{})", bb.width, bb.height, bb.x, bb.y);
            if let Ok((x, y)) = seam_input::cursor_position() {
                println!("    cursor   ({x}, {y})");
            }
        }
        Err(e) => println!("    could not read displays: {e}"),
    }

    if let Some(report) = seam_input::permission_report() {
        println!("\n  permissions");
        for (what, granted, where_to) in report {
            if granted {
                println!("    {what:<16} granted");
            } else {
                println!("    {what:<16} NOT GRANTED — {where_to}");
            }
        }
    }

    let store = store::load_peers(dir);
    println!("\n  paired peers   {}", store.len());
    for (id, peer) in store.iter() {
        println!("    {id}  {}", peer.name);
    }

    print!("\n  network        ");
    std::io::stdout().flush().ok();
    match Endpoint::bind(Arc::new(Identity::generate()?), "0.0.0.0:0".parse()?) {
        Ok(ep) => {
            println!("ok — bound a QUIC socket on {}", ep.local_addr()?);
            ep.close();
        }
        Err(e) => println!("FAILED — {e}"),
    }

    print!("  discovery      ");
    std::io::stdout().flush().ok();
    match Discovery::new() {
        Ok(d) => {
            match d.browse() {
                Ok(_) => println!("ok — mDNS is available on this machine"),
                Err(e) => println!("FAILED — {e}"),
            }
            d.shutdown();
        }
        Err(e) => println!("FAILED — {e}"),
    }

    match licence::stored(dir) {
        Some(l) => println!("  licence       {} ({})", l.name, if l.expires_day == 0 { "no expiry".to_owned() } else { format!("day {}", l.expires_day) }),
        None => println!("  licence       MISSING — run 'seam activate <your-licence>'"),
    }

    report_input_forwarding();

    // Windows only: whether injection survives elevated-window focus. UIPI failures are
    // silent, so this is the one place they can be seen before they are felt.
    #[cfg(target_os = "windows")]
    {
        println!("\n  elevation");
        if seam_input::windows::is_elevated() {
            println!("    elevated — injection reaches admin windows and Task Manager");
        } else {
            println!("    NOT ELEVATED — the pointer and keyboard freeze whenever an");
            println!("    elevated window (admin PowerShell, Task Manager) has focus.");
            println!("    'seam run' offers to fix this with one UAC prompt.");
        }
    }

    Ok(())
}


/// Report whether input can actually be *withheld*, not merely seen.
///
/// Split out of `doctor` for length, but it earns its own name: this is the check for the
/// failure that does not look like one. With Input Monitoring but not Accessibility, macOS
/// downgrades the event tap to listen-only. Input is captured and forwarded correctly while
/// this machine goes on acting on it, so the pointer appears to be on two screens at once.
fn report_input_forwarding() {
    println!("\n  input forwarding");
    match seam_input::permission_report() {
        Some(report) => {
            let granted = |name| {
                report.iter().find(|(what, _, _)| *what == name).is_some_and(|(_, ok, _)| *ok)
            };
            match (granted("capture input"), granted("inject input")) {
                (true, true) => println!("    ready — input can be captured, withheld and forwarded"),
                (true, false) => {
                    println!("    MIRRORS INSTEAD OF SWITCHING — Accessibility is not granted.");
                    println!("    seam can see and forward input, but cannot stop this machine");
                    println!("    acting on it, so the pointer moves here AND on the other screen.");
                    println!("    Grant System Settings > Privacy & Security > Accessibility,");
                    println!("    then restart seam. macOS grants this per binary, so a newly");
                    println!("    downloaded release needs it again.");
                }
                (false, _) => {
                    println!("    NOT WORKING — Input Monitoring is not granted, so seam cannot");
                    println!("    see input at all. Grant it, then restart seam.");
                }
            }
        }
        None => println!("    ready — this platform needs no input permissions"),
    }

}

// ---------------------------------------------------------------- discover

/// Collect peers for `window`, reporting each as it appears.
///
/// `exclude` drops this machine's own advertisement. A browse handle only knows to filter
/// itself out when the *same* handle is also advertising, and the dialler's handle is a
/// separate one — so without this a machine discovers itself and offers to pair with it.
async fn collect_peers(
    window: Duration,
    exclude: Option<seam_proto::PeerId>,
    on_found: impl Fn(&DiscoveredPeer),
) -> Vec<DiscoveredPeer> {
    let Ok(discovery) = Discovery::new() else {
        return Vec::new();
    };
    let Ok(stream) = discovery.browse() else {
        discovery.shutdown();
        return Vec::new();
    };

    let mut found: Vec<DiscoveredPeer> = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(DiscoveryEvent::Found(peer))) => {
                if peer.advertised_peer_id.is_some() && peer.advertised_peer_id == exclude {
                    continue;
                }
                if !found.iter().any(|p| p.instance == peer.instance) {
                    on_found(&peer);
                    found.push(*peer);
                }
            }
            Ok(Some(DiscoveryEvent::Lost { instance })) => {
                found.retain(|p| p.instance != instance);
            }
            Ok(None) | Err(_) => break,
        }
    }
    discovery.shutdown();
    found
}

async fn discover(window: Duration, own: seam_proto::PeerId) -> Result<()> {
    println!("Watching for other seam machines for {}s...\n", window.as_secs());
    let found = collect_peers(window, Some(own), |peer| {
        let version =
            peer.advertised_protocol.map_or_else(|| "unknown".to_owned(), |v| format!("v{v}"));
        let addresses: Vec<String> = peer.addresses.iter().map(ToString::to_string).collect();
        println!("  {}", peer.name);
        println!(
            "    claims id {}   protocol {version}",
            peer.advertised_peer_id.map_or_else(|| "?".into(), |id| id.to_string())
        );
        println!("    at {}", addresses.join(", "));
    })
    .await;

    if found.is_empty() {
        println!("  no other seam machines found.\n");
        println!("  If one should be running, check that both are on the same network and");
        println!("  that mDNS is not blocked. seam never needs an IP address typed in.");
    } else {
        println!("\nFound {}. Pair with: seam pair <name>", found.len());
    }
    Ok(())
}

// ---------------------------------------------------------------- pair

async fn pair(
    dir: &std::path::Path,
    identity: Arc<Identity>,
    peer: Option<String>,
    at: Option<String>,
    timeout: Duration,
) -> Result<()> {
    let mut store = store::load_peers(dir);
    let endpoint = Arc::new(Endpoint::bind(Arc::clone(&identity), "0.0.0.0:0".parse()?)?);
    let port = endpoint.local_addr()?.port();
    let name = Discovery::default_display_name(identity.peer_id());

    let mut discovery = Discovery::new()?;
    discovery.advertise(&name, identity.peer_id(), identity.fingerprint(), port)?;

    let link = if let Some(address) = at {
        let target: SocketAddr =
            address.parse().with_context(|| format!("{address:?} is not a HOST:PORT address"))?;
        println!("Dialling {target} directly...");
        endpoint.connect(target).await.map_err(anyhow::Error::from)
    } else if let Some(query) = peer {
        dial_for_pairing(&endpoint, identity.peer_id(), &query, timeout).await
    } else {
        {
            println!("Waiting for the other machine to connect.\n");
            println!("  On the other machine run either:");
            println!("      seam pair {name}");
            println!("  or, if its firewall blocks discovery, dial this machine directly:");
            for address in local_addresses(port) {
                println!("      seam pair --at {address}");
            }
            println!();
            match tokio::time::timeout(timeout, endpoint.accept()).await {
                Ok(Some(link)) => link.map_err(anyhow::Error::from),
                Ok(None) => bail!("this machine stopped listening before anyone connected"),
                Err(_) => bail!(
                    "nobody connected within {}s. Run `seam pair {name}` on the other machine.",
                    timeout.as_secs()
                ),
            }
        }
    };
    discovery.shutdown();
    let link = link?;

    let code = link.pairing_code()?;
    println!("\n  Pairing with {}\n", link.remote_address());
    println!("      ┌───────────────┐");
    println!("      │   {code}   │", code = code.to_display_string());
    println!("      └───────────────┘\n");
    println!("  Check the other machine shows the SAME code, then confirm.");
    println!("  If the codes differ, something is intercepting the connection — say no.\n");

    if !confirm("  Do the codes match? [y/N] ")? {
        link.close("pairing declined");
        bail!("pairing cancelled — nothing was saved");
    }

    // The name is what the peer advertises; it is cosmetic and can change freely.
    let peer_name = format!("{}", link.peer_id());
    store.trust(link.peer_fingerprint(), peer_name);
    store::save_peers(dir, &store)?;

    println!("\n  Paired. This machine will now connect to {} automatically.", link.peer_id());
    println!("  Run `seam run` on both machines.");
    link.close("paired");
    Ok(())
}

async fn dial_for_pairing(
    endpoint: &Endpoint,
    own: seam_proto::PeerId,
    query: &str,
    timeout: Duration,
) -> Result<Link> {
    println!("Looking for {query:?} on the network...");
    let needle = query.trim().to_lowercase();
    let window = timeout.min(Duration::from_secs(10));
    let found = collect_peers(window, Some(own), |_| {}).await;

    let candidate = found
        .iter()
        .find(|p| {
            p.name.to_lowercase() == needle
                || p.advertised_peer_id.is_some_and(|id| id.to_string().starts_with(&needle))
        })
        .with_context(|| {
            let names: Vec<_> = found.iter().map(|p| p.name.clone()).collect();
            if names.is_empty() {
                format!(
                    "no seam machines found on the network. Start `seam pair` on {query:?} first."
                )
            } else {
                format!("no machine called {query:?}. Found: {}", names.join(", "))
            }
        })?;

    // Race every advertised address and take the first that answers: a machine with both
    // Wi-Fi and Ethernet advertises several, and picking one is a coin flip.
    connect_any(endpoint, &candidate.addresses).await
}

/// Dial every address concurrently, keep the first success.
async fn connect_any(endpoint: &Endpoint, addresses: &[SocketAddr]) -> Result<Link> {
    let mut last: Option<TransportError> = None;
    for addr in addresses {
        match endpoint.connect(*addr).await {
            Ok(link) => return Ok(link),
            Err(e) => {
                tracing::debug!(%addr, error = %e, "address did not answer");
                last = Some(e);
            }
        }
    }
    if let Some(e) = last {
        return Err(e.into());
    }
    bail!("that machine advertised no reachable address")
}

/// Addresses another machine could dial this one on.
fn local_addresses(port: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for probe in ["8.8.8.8:9", "192.168.0.1:9", "10.0.0.1:9"] {
        let Ok(target) = probe.parse::<SocketAddr>() else { continue };
        let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else { continue };
        if socket.connect(target).is_ok()
            && let Ok(local) = socket.local_addr()
            && !out.iter().any(|a: &SocketAddr| a.ip() == local.ip())
        {
            out.push(SocketAddr::new(local.ip(), port));
        }
    }
    out
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).context("reading your answer")?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

// ---------------------------------------------------------------- peers / forget

fn peers(dir: &std::path::Path) {
    let store = store::load_peers(dir);
    if store.is_empty() {
        println!("No paired machines yet. Run `seam pair` on both machines.");
        return;
    }
    println!("Paired machines:\n");
    for (id, peer) in store.iter() {
        println!("  {id}  {}", peer.name);
    }
}

fn forget(dir: &std::path::Path, query: &str) -> Result<()> {
    let mut store = store::load_peers(dir);
    let fingerprint = store::resolve_peer(&store, query)?;
    let removed = store.forget(fingerprint.peer_id());
    store::save_peers(dir, &store)?;
    match removed {
        Some(peer) => println!("Forgot {} ({}).", peer.name, fingerprint.peer_id()),
        None => println!("Nothing to forget."),
    }
    Ok(())
}

// ---------------------------------------------------------------- daemon

async fn daemon(
    dir: &std::path::Path,
    identity: Arc<Identity>,
    port: u16,
    connect: Vec<String>,
) -> Result<()> {
    // Clear any input state a previous run left behind, before doing anything else.
    //
    // CGAssociateMouseAndMouseCursorPosition(false) **survives process death**: if seam
    // ever exits while the pointer is on a peer — Ctrl+C at the wrong moment, a crash, a
    // kill — this machine's cursor stays detached from its mouse, and every later run
    // inherits a Mac whose mouse does not work. This is precisely how another KVM
    // stranded the pointer on this machine, and starting from a known-good state is the
    // only reliable defence, because the process that broke it is already gone.
    seam_input::release_input();

    let store: SharedTrust = Arc::new(std::sync::RwLock::new(store::load_peers(dir)));
    let endpoint =
        match bind_or_show_running(dir, &identity, port)? {
            std::ops::ControlFlow::Continue(endpoint) => endpoint,
            std::ops::ControlFlow::Break(()) => return Ok(()),
        };
    let bound = endpoint.local_addr()?;
    let name = Discovery::default_display_name(identity.peer_id());

    let mut discovery = Discovery::new()?;
    discovery.advertise(&name, identity.peer_id(), identity.fingerprint(), bound.port())?;

    let paired_count = store.read().map_or(0, |s| s.len());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %name,
        id = %identity.peer_id(),
        %bound,
        peers = paired_count,
        "seam is running"
    );
    if paired_count == 0 {
        tracing::info!(
            "no paired machines yet — when another seam appears on this network, both \
             screens will show the same six digits; confirm them to pair"
        );
    }

    // Real screen size of each peer, learned from its `Hello`. Until a peer reports its
    // geometry it is not placed in the layout at all: guessing a size puts the screen
    // boundary at a coordinate that exists on neither machine, which looks exactly like
    // "the pointer will not leave the screen".
    let geometry: Geometry = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    let clipboard: Clipboard = Arc::new(tokio::sync::Mutex::new(ClipboardState::default()));

    // Every authorised link, so captured pointer motion can be sent to all of them.
    let links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));

    // Dial out where asked. Outbound is what works when an inbound firewall blocks
    // discovery, so this is the path that forms a link on a locked-down machine.
    for address in &connect {
        let Ok(target) = address.parse::<SocketAddr>() else {
            tracing::warn!(%address, "not a HOST:PORT address");
            continue;
        };
        // Keep trying, forever, rather than once at startup.
        //
        // A single attempt means the machines can only ever be started in one order: a
        // client launched before its server logged "could not connect" and then sat there
        // doing nothing for the rest of the session, which reads as pairing being broken.
        // Nothing about seam should care which machine was switched on first.
        //
        // The same loop reconnects after the link drops, so a server restart no longer
        // strands every client either.
        spawn_reconnector(
            target,
            port,
            Arc::clone(&endpoint),
            Arc::clone(&store),
            Arc::clone(&links),
            Arc::clone(&geometry),
            Arc::clone(&clipboard),
        );
    }

    // A machine that was told to dial somewhere joined a fleet: it is a client, and stays
    // one across restarts. Recorded here rather than inferred later, so the answer cannot
    // change when connections race.
    if !connect.is_empty() && !dir.join("joined").exists() {
        let _ = std::fs::write(dir.join("joined"), "");
        let _ = std::fs::remove_file(dir.join("first-machine"));
    }

    load_settings_into_ui(dir);

    start_update_watch();
    start_auto_dial(&discovery, &store, &links, &geometry, &clipboard, &endpoint);

    #[cfg(target_os = "windows")]
    recover_this_machine();

    start_ui_server(
        dir.to_path_buf(),
        name.clone(),
        identity.peer_id(),
        port,
        Arc::clone(&links),
        Arc::clone(&store),
        Some(Arc::clone(&endpoint)),
    );
    start_pointer_forwarding(Arc::clone(&links), Arc::clone(&geometry), dir.to_path_buf());

    start_clipboard_sync(Arc::clone(&links), Arc::clone(&clipboard));
    start_input_watchdog();

    let accepting = start_accepting(
        Arc::clone(&endpoint),
        Arc::clone(&store),
        Arc::clone(&links),
        Arc::clone(&geometry),
        Arc::clone(&clipboard),
    );

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutting down");
    remove_ui_port_note_if_ours(dir);
    // Never exit while this machine is still withholding its own input.
    seam_input::release_input();
    tracing::info!("this machine's input restored");
    discovery.shutdown();
    endpoint.close();
    accepting.abort();
    Ok(())
}

/// Receive motion from a peer and reproduce it on this machine.
async fn receive_from(
    link: Arc<Link>,
    geometry: Geometry,
    clipboard: Clipboard,
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
) {
    let peer_id = link.peer_id();
    receive_from_inner(link, geometry, clipboard, Arc::clone(&links)).await;

    // Forget the peer on every exit path.
    //
    // Nothing used to remove a closed link from the shared list, so a machine that had
    // quit stayed listed as connected forever — the fleet page showed corpses, and the
    // clipboard fan-out kept sending to them. Doing it here rather than at each `return`
    // means a future early exit cannot reintroduce the leak.
    links.lock().await.retain(|l| l.peer_id() != peer_id);
    if let Ok(mut places) = UI_PLACES.lock() {
        places.retain(|(p, _)| *p != peer_id);
    }
    if let Ok(mut roles) = UI_ROLES.lock() {
        roles.retain(|(p, _)| *p != peer_id);
    }
    if let Ok(mut focus) = UI_FOCUS.lock()
        && *focus == Some(peer_id)
    {
        // Never leave the pointer pointed at a machine that is gone.
        *focus = None;
    }
    tracing::info!(peer = %peer_id, "peer disconnected; removed from the fleet");
}

/// Close the link if the peer's `Hello` names another protocol. Enforced, not
/// advisory: a v1 and a v2 machine spent a morning connected, exchanging payloads each
/// interpreted differently — mixed protocols must fail at the handshake, loudly, not
/// at the first garbled paste.
fn hello_speaks_another_protocol(link: &Link, peer: seam_proto::PeerId, theirs: u16) -> bool {
    if theirs == seam_proto::PROTOCOL_VERSION {
        return false;
    }
    tracing::warn!(
        %peer,
        ours = seam_proto::PROTOCOL_VERSION,
        theirs,
        "peer speaks a different protocol; closing the link — update every machine to the same version"
    );
    link.close("protocol mismatch — update every machine");
    true
}

/// The receive loop itself. Split so `receive_from` owns the cleanup for every exit.
async fn receive_from_inner(
    link: Arc<Link>,
    geometry: Geometry,
    clipboard: Clipboard,
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
) {
    tokio::spawn(receive_reliable(Arc::clone(&link), geometry, clipboard, links));
    let peer = link.peer_id();
    let Ok(desktop) = seam_input::desktop() else {
        tracing::warn!(%peer, "cannot read this machine's displays, so incoming motion is ignored");
        return;
    };

    let mut received: u64 = 0;
    let mut rejected: u64 = 0;
    tracing::info!(%peer, "ready to receive input from this peer");

    loop {
        match link.recv_datagram().await {
            Ok(seam_proto::Frame::Leave { .. }) => {
                // The pointer went somewhere else: hide this machine's cursor, exactly
                // as the server hides its own. Reveal happens on the next arriving
                // motion, on link loss, and at startup — fail open, always.
                // The pointer went to some other machine. A client cannot know which, so
                // it records the peer that told it — the machine holding the session —
                // rather than claiming the pointer is still here.
                if let Ok(mut focus) = UI_FOCUS.lock() {
                    *focus = Some(peer);
                }
                #[cfg(target_os = "windows")]
                {
                    seam_input::windows::conceal_cursor();
                    // Anything still held belongs to the session that just ended.
                    seam_input::windows::release_everything();
                    tracing::info!("pointer left this machine; cursor hidden, held keys released");
                }
            }
            Ok(seam_proto::Frame::Motion(motion)) => {
                // A peer that sends motion is a machine that captures input. Learned by
                // observation rather than announced: the protocol carries no capability
                // field, and inventing one from a guess is how a UI starts lying.
                if let Ok(mut roles) = UI_ROLES.lock()
                    && !roles.iter().any(|(p, _)| *p == peer)
                {
                    roles.push((peer, "shares input"));
                }
                #[cfg(target_os = "windows")]
                seam_input::windows::reveal_if_concealed();
                // Input arriving means the pointer is HERE. A client has no focus graph,
                // so without this its page always claimed the pointer was local and its
                // own screen stayed highlighted no matter where the pointer really was.
                if let Ok(mut focus) = UI_FOCUS.lock() {
                    *focus = None;
                }
                let (px, py) = motion.cursor.to_px();
                // The sender's coordinates are its own; clamp onto a real display here so
                // a mismatched resolution can never strand the pointer somewhere invisible.
                let (x, y) = desktop.clamp_onto_a_display(px, py);
                match seam_input::inject_motion(x, y) {
                    Ok(()) => {
                        received = received.wrapping_add(1);
                        if received == 1 {
                            tracing::info!(%peer, x, y, "moving this machine's pointer");
                        }
                    }
                    Err(e) => {
                        rejected = rejected.wrapping_add(1);
                        if rejected == 1 || rejected.is_multiple_of(500) {
                            tracing::warn!(%peer, rejected, "could not move the pointer: {e}");
                        }
                    }
                }
            }
            Ok(seam_proto::Frame::Button(b)) => {
                apply_button(peer, b.button.to_u8(), b.press.is_down());
            }
            Ok(seam_proto::Frame::Scroll(sc)) => {
                if let Err(e) = seam_input::inject_scroll(sc.dx, sc.dy) {
                    tracing::warn!(%peer, "could not scroll: {e}");
                }
            }
            Ok(seam_proto::Frame::Key(k)) => {
                // Key events arrive on a reliable stream, not here; handled in the
                // reliable loop below.
                let _ = k;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::info!(%peer, "link closed: {e}");
                #[cfg(target_os = "windows")]
                {
                    // The link may have died between a DOWN and its UP. Leaving the key
                    // held is how "some keys stopped working" happens.
                    seam_input::windows::release_everything();
                    seam_input::windows::reveal_cursor();
                }
                return;
            }
        }
    }
}

/// Apply a clipboard image from a peer, then relay it around the star.
///
/// The mirror of the text arm above it, split out only for length: generation check,
/// write, remember-what-was-written so the poller does not echo it, then forward to
/// every peer except the one it came from, generation unchanged so it cannot loop.
async fn apply_clipboard_image(
    peer: seam_proto::PeerId,
    generation: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    clipboard: &Clipboard,
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
) {
    let mut state = clipboard.lock().await;
    if state.already_applied(peer, generation) {
        tracing::debug!(%peer, generation, "clipboard image ignored as an echo or older");
        return;
    }
    // The wire carries deflate (protocol v2); the clipboard and the signature both
    // want raw pixels. The relay below forwards the compressed original untouched —
    // decompressing just to recompress would tax the hub for nothing.
    let Some(raw) = inflate_pixels(&rgba, width, height) else {
        tracing::warn!(%peer, width, height, "clipboard image discarded: pixels did not decompress to the claimed size");
        return;
    };
    match seam_input::clipboard::write_image(width, height, &raw) {
        Ok(()) => {
            tracing::info!(%peer, width, height, bytes = rgba.len(), "clipboard image received");
            note_transfer("image received", format!("{width} x {height}"));
            state.record(peer, generation);
            state.image_sig = Some(image_signature(width, height, &raw));
            state.last_seen = None;
            // Reissued under this machine's counter — same reasoning as the text relay.
            state.generation += 1;
            let relay = seam_proto::Frame::ClipboardImage {
                seq: 0,
                generation: state.generation,
                width,
                height,
                rgba,
            };
            drop(state);
            for other in links.lock().await.iter() {
                if other.peer_id() == peer {
                    continue;
                }
                if let Err(e) = other.send_reliable(&relay).await {
                    tracing::warn!(
                        peer = %other.peer_id(),
                        "could not relay the clipboard image: {e}"
                    );
                }
            }
        }
        Err(e) => tracing::warn!(%peer, "could not set the clipboard image: {e}"),
    }
}

/// Switch a peer on or off from the UI, matched by its short id (what the page shows).
/// Answer one fleet-page request. Split out of the accept loop for length.
/// What a request handler needs to know about this machine. Grouped rather than passed
/// as eight parameters: the count was a lint, but the real point is that these travel
/// together and always will.
struct Serving<'a> {
    /// The loopback port the page itself is on.
    ui_port: u16,
    /// The port peers connect to — what a joining machine needs, and not the same number.
    seam_port: u16,
    name: &'a str,
    id: seam_proto::PeerId,
    links: &'a Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    ui_port_note: &'a std::path::Path,
    store: &'a SharedTrust,
    /// Absent in the UI-only test server; pairing actions answer 503 without it.
    endpoint: Option<&'a Arc<seam_transport::Endpoint>>,
    dir: &'a std::path::Path,
}

/// The pairing page's slice of `/state`: who is visible, and the ceremony in flight.
fn pairing_state_json() -> (String, String) {
    use std::fmt::Write as _;
    let mut discovered = String::new();
    if let Ok(seen) = UI_DISCOVERED.lock() {
        for machine in seen.iter() {
            if !discovered.is_empty() {
                discovered.push(',');
            }
            let _ = write!(
                discovered,
                r#"{{"id":"{}","addr":"{}","trusted":{}}}"#,
                machine.short.replace('"', "'"),
                machine.addr,
                machine.trusted
            );
        }
    }
    let pairing = UI_PAIRING
        .lock()
        .ok()
        .and_then(|slot| {
            slot.as_ref().map(|pending| {
                let state = match pending.outcome {
                    None => "showing",
                    Some(true) => "paired",
                    Some(false) => "declined",
                };
                format!(
                    r#"{{"code":"{}","with":"{}","state":"{state}"}}"#,
                    pending.code,
                    pending.with.replace('"', "'")
                )
            })
        })
        .unwrap_or_else(|| "null".to_owned());
    (discovered, pairing)
}

/// Answer the pairing actions: confirm, decline, or dial a listed machine.
fn pair_action(path: &str, ctx: &Serving<'_>) -> (&'static str, &'static str, String) {
    if path == "/action/pair/confirm" {
        // "They match": trust is written and saved before the link closes, so the peer's
        // very next dial connects as a member of the fleet.
        let ok = if let Ok(mut slot) = UI_PAIRING.lock()
            && let Some(pending) = slot.as_mut()
            && pending.outcome.is_none()
        {
            let saved = if let Ok(mut trust) = ctx.store.write() {
                trust.trust(pending.link.peer_fingerprint(), pending.with.clone());
                store::save_peers(ctx.dir, &trust).is_ok()
            } else {
                false
            };
            pending.link.close("paired");
            pending.outcome = Some(true);
            tracing::info!(peer = %pending.with, "paired — both screens agreed on the six digits");
            saved
        } else {
            false
        };
        return ("200 OK", "application/json", format!(r#"{{"ok":{ok}}}"#));
    }
    if path == "/action/pair/decline" {
        let ok = if let Ok(mut slot) = UI_PAIRING.lock()
            && let Some(pending) = slot.as_mut()
            && pending.outcome.is_none()
        {
            pending.link.close("pairing declined");
            pending.outcome = Some(false);
            tracing::warn!(peer = %pending.with, "pairing declined — the numbers did not match, nothing was trusted");
            true
        } else {
            false
        };
        return ("200 OK", "application/json", format!(r#"{{"ok":{ok}}}"#));
    }
    // Pair… on a listed machine: dial it and put the six digits on both screens.
    let Some(endpoint) = ctx.endpoint else {
        return ("503 Service Unavailable", "application/json", r#"{"ok":false}"#.to_owned());
    };
    let short = path.trim_start_matches("/action/pair/").to_owned();
    let target = UI_DISCOVERED
        .lock()
        .ok()
        .and_then(|seen| seen.iter().find(|m| m.short.starts_with(&short)).map(|m| m.addr));
    let ok = target.is_some();
    if let Some(addr) = target {
        let endpoint = Arc::clone(endpoint);
        tokio::spawn(async move {
            match endpoint.connect(addr).await {
                Ok(link) => park_pairing(link),
                Err(e) => tracing::warn!(%addr, "could not reach the machine to pair: {e}"),
            }
        });
    }
    ("200 OK", "application/json", format!(r#"{{"ok":{ok}}}"#))
}

async fn route(
    request: &str,
    path: &str,
    ctx: &Serving<'_>,
) -> (&'static str, &'static str, String) {
    let (port, seam_port, name, id, links, ui_port_note) =
        (ctx.ui_port, ctx.seam_port, ctx.name, ctx.id, ctx.links, ctx.ui_port_note);
    let method = request.split_whitespace().next().unwrap_or("GET");

    // The origin gate lives HERE, ahead of every branch, so no future route can be added
    // outside it. Extracting this router once already dropped this check silently — the
    // only reason it was caught is that the compiler noticed the function had no callers.
    if !request_is_local(request, port) {
        tracing::warn!(path, "refused a fleet-page request from another origin");
        return ("403 Forbidden", "text/plain", "refused".to_owned());
    }

    if method == "POST" && path == "/action/quit" {
        // Respond first, then die: the page needs the reply to close its tab.
        tracing::info!("quit requested from the fleet page");
        let note = ui_port_note.to_path_buf();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            seam_input::release_input();
            // Only take the note down if it still names this process. A takeover
            // quits the old seam politely, and by the time this timer fires the
            // replacement has often already written its own port here — deleting it
            // unconditionally left the survivor healthy but unfindable.
            let noted = std::fs::read_to_string(&note)
                .ok()
                .and_then(|text| text.trim().parse::<u16>().ok());
            if noted == Some(port) {
                let _ = std::fs::remove_file(note);
            }
            std::process::exit(0);
        });
        ("200 OK", "application/json", r#"{"ok":true}"#.to_owned())
    } else if method == "POST" && path.starts_with("/action/pair/") {
        pair_action(path, ctx)
    } else if method == "POST" && path.starts_with("/action/startup/") {
        let on = path.ends_with("/on");
        let now = set_start_at_login(on);
        tracing::info!(requested = on, actual = now, "start at login changed");
        ("200 OK", "application/json", format!(r#"{{"ok":true,"on":{now}}}"#))
    } else if method == "POST" && path.starts_with("/action/share/") {
        let mut parts = path.trim_start_matches("/action/share/").split('/');
        let kind = parts.next().unwrap_or("");
        let on = parts.next() == Some("on");
        if let Ok(mut shares) = UI_SHARES.lock() {
            match kind {
                "text" => shares.0 = on,
                "images" => shares.1 = on,
                "files" => shares.2 = on,
                _ => {}
            }
        }
        persist_settings();
        tracing::info!(kind, on, "clipboard sharing changed from the page");
        ("200 OK", "application/json", r#"{"ok":true}"#.to_owned())
    } else if method == "POST" && path.starts_with("/action/layout/") {
        let mut parts = path.trim_start_matches("/action/layout/").split('/');
        let peer = parts.next().unwrap_or("");
        let edge = parts.next().unwrap_or("");
        let anchor = parts.next().unwrap_or("self");
        let dir = ui_port_note.parent().unwrap_or(ui_port_note);
        let ok = set_layout(dir, peer, edge, anchor);
        ("200 OK", "application/json", format!(r#"{{"ok":{ok}}}"#))
    } else if method == "POST" && path.starts_with("/action/peer/") {
        let mut parts = path.trim_start_matches("/action/peer/").split('/');
        let target = parts.next().unwrap_or("").to_owned();
        let enable = parts.next() == Some("enable");
        set_peer_enabled(&target, enable);
        ("200 OK", "application/json", r#"{"ok":true}"#.to_owned())
    } else if method == "POST" && path == "/action/release" {
        UI_RELEASE.store(true, std::sync::atomic::Ordering::Relaxed);
        ("200 OK", "application/json", r#"{"ok":true}"#.to_owned())
    } else if method == "POST" && path == "/action/activate" {
        // The licence arrives in the body; a key in a URL would end up in logs.
        let key = request.split("\r\n\r\n").nth(1).unwrap_or("").trim();
        let dir = ui_port_note.parent().unwrap_or(ui_port_note);
        match licence::activate(dir, key) {
            Ok(l) => (
                "200 OK",
                "application/json",
                format!(r#"{{"ok":true,"name":"{}"}}"#, l.name.replace('"', "'")),
            ),
            Err(e) => (
                "200 OK",
                "application/json",
                format!(r#"{{"ok":false,"error":"{}"}}"#, e.to_string().replace('"', "'")),
            ),
        }
    } else if path == "/join" {
        // The only thing a joining machine trusts this server for: which version to
        // fetch. The bytes come from GitHub over TLS and are checked against that
        // release's own published checksums — a binary served from a machine on the LAN
        // would be unauthenticated and unverifiable.
        (
            "200 OK",
            "application/json",
            format!(
                r#"{{"version":"{}","repo":"amor-openmind/seam-releases"}}"#,
                env!("CARGO_PKG_VERSION")
            ),
        )
    } else if path == "/state" {
        ("200 OK", "application/json", ui_state_json(name, id, port, seam_port, ui_port_note.parent().unwrap_or(ui_port_note), links)
            .await)
    } else if let Some((_, page, ctype)) =
        UI_PAGES.iter().find(|(route, _, _)| *route == path)
    {
        ("200 OK", *ctype, (*page).to_owned())
    } else {
        ("404 Not Found", "text/plain", "not found".to_owned())
    }
}

/// Load the saved settings into the state the UI and the input path read.
///
/// Without this, switching a machine off in the fleet page was forgotten the moment seam
/// stopped — indistinguishable, from a chair, from the switch not working at all.
fn load_settings_into_ui(dir: &std::path::Path) {
    let settings = store::load_settings(dir);
    if let Ok(mut off) = UI_DISABLED.lock() {
        (*off).clone_from(&settings.disabled_peers);
    }
    if let Ok(mut shares) = UI_SHARES.lock() {
        *shares = (settings.share_text, settings.share_images, settings.share_files);
    }
    if let Ok(mut home) = UI_HOME.lock() {
        *home = Some(dir.to_path_buf());
    }
    tracing::info!(
        text = settings.share_text,
        images = settings.share_images,
        files = settings.share_files,
        disabled = settings.disabled_peers.len(),
        "settings loaded"
    );
}

fn persist_settings() {
    let Ok(home) = UI_HOME.lock() else { return };
    let Some(dir) = home.as_ref() else { return };
    let (share_text, share_images, share_files) =
        UI_SHARES.lock().map_or((true, true, true), |shares| *shares);
    let disabled_peers = UI_DISABLED.lock().map(|off| off.clone()).unwrap_or_default();
    store::save_settings(
        dir,
        &store::Settings { disabled_peers, share_text, share_images, share_files },
    );
}

fn set_peer_enabled(short_id: &str, enable: bool) {
    let Ok(mut off) = UI_DISABLED.lock() else { return };
    let matches_short = |peer: &seam_proto::PeerId| peer.to_string().starts_with(short_id);
    if enable {
        off.retain(|peer| !matches_short(peer));
        tracing::info!(peer = short_id, "peer enabled from the fleet page");
        drop(off);
        persist_settings();
    } else if let Ok(places) = UI_PLACES.lock()
        && let Some((peer, _)) = places.iter().find(|(peer, _)| matches_short(peer))
        && !off.contains(peer)
    {
        off.push(*peer);
        tracing::info!(peer = short_id, "peer disabled from the fleet page");
        drop(off);
        persist_settings();
    }
}

/// Apply clipboard files from a peer: spool them locally, point this machine's
/// clipboard at the spooled copies, then relay around the star like text and images.
async fn apply_clipboard_files(
    peer: seam_proto::PeerId,
    generation: u64,
    entries: Vec<(String, Vec<u8>)>,
    clipboard: &Clipboard,
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
) {
    let mut state = clipboard.lock().await;
    if state.already_applied(peer, generation) {
        tracing::debug!(%peer, generation, "clipboard files ignored as an echo or older");
        return;
    }
    // The wire carries deflate per file (protocol v2); the spool wants real bytes. The
    // relay below forwards the compressed originals untouched.
    let mut raw_entries = Vec::with_capacity(entries.len());
    for (name, wire) in &entries {
        let Some(raw) = inflate_file_bytes(wire) else {
            tracing::warn!(%peer, %name, "clipboard files discarded: an entry did not decompress to its claimed size");
            return;
        };
        raw_entries.push((name.clone(), raw));
    }
    let spooled = match spool_clipboard_files(generation, &raw_entries) {
        Ok(tops) => tops,
        Err(e) => {
            tracing::warn!(%peer, "could not store the received files: {e}");
            return;
        }
    };
    match seam_input::clipboard::write_file_list(&spooled) {
        Ok(()) => {
            let bytes: usize = raw_entries.iter().map(|(_, b)| b.len()).sum();
            tracing::info!(%peer, files = entries.len(), bytes, "clipboard files received");
            note_transfer("files received", format!("{} files, {} KB", entries.len(), bytes / 1024));
            state.record(peer, generation);
            state.files_sig = Some(files_signature(&spooled));
            state.last_seen = None;
            state.image_sig = None;
            // Reissued under this machine's counter — same reasoning as the text relay.
            state.generation += 1;
            let relay = seam_proto::Frame::ClipboardFiles {
                seq: 0,
                generation: state.generation,
                entries,
            };
            drop(state);
            for other in links.lock().await.iter() {
                if other.peer_id() == peer {
                    continue;
                }
                if let Err(e) = other.send_reliable(&relay).await {
                    tracing::warn!(
                        peer = %other.peer_id(),
                        "could not relay the clipboard files: {e}"
                    );
                }
            }
        }
        Err(e) => tracing::warn!(%peer, "could not point the clipboard at the files: {e}"),
    }
}

// ---------------------------------------------------------------- ui

/// The port seam listens on unless told otherwise.
///
/// Stable rather than OS-assigned. A random port means a peer that was told an address
/// once can never find this machine again, and discovery becomes the only way in — so any
/// hiccup there looks like seam being broken. Fixed by default, overridable.
const DEFAULT_PORT: u16 = 24810;

/// Which clipboard kinds this machine shares: (text, images, files).
static UI_SHARES: std::sync::Mutex<(bool, bool, bool)> = std::sync::Mutex::new((true, true, true));

/// Where settings are written, so an action handler can persist without threading a path
/// through every layer.
static UI_HOME: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// The most recent clipboard movement, for the fleet page. Text is cheap and instant;
/// an image or a folder is not, and a page that shows nothing while megabytes move is
/// indistinguishable from one that is broken.
static UI_TRANSFER: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// The daemon's trust store, shared and writable: pairing from the page adds a peer
/// while every connection path keeps checking it. Reads vastly outnumber writes.
type SharedTrust = Arc<std::sync::RwLock<seam_transport::TrustStore>>;

/// A machine seen on the network right now, paired or not, for the pairing page.
#[derive(Clone)]
struct DiscoveredMachine {
    /// DNS-SD instance name — the only key a removal event carries.
    instance: String,
    short: String,
    addr: std::net::SocketAddr,
    trusted: bool,
}

static UI_DISCOVERED: std::sync::Mutex<Vec<DiscoveredMachine>> = std::sync::Mutex::new(Vec::new());

/// An in-flight pairing: the parked link and the six digits both screens are showing.
///
/// The link is held open, unregistered and unauthorised, for exactly as long as the
/// person needs to compare two numbers. Everything else about it stays quarantined.
struct PendingPairing {
    code: String,
    with: String,
    link: Arc<Link>,
    /// `None` while the digits are showing; the page keeps the outcome visible after.
    outcome: Option<bool>,
}

static UI_PAIRING: std::sync::Mutex<Option<PendingPairing>> = std::sync::Mutex::new(None);

/// Park an unpaired link and show its six digits, instead of hanging up on it.
///
/// This is the whole command-free join: a machine nobody has vouched for connects, the
/// same code derives from the encrypted channel on both ends, and two people compare
/// two numbers. Refusing outright — which is what the accept path used to do — meant
/// the only way in was a shell command nobody should have to know.
fn park_pairing(link: Link) {
    let Ok(code) = link.pairing_code() else {
        link.close("pairing code unavailable");
        return;
    };
    let Ok(mut slot) = UI_PAIRING.lock() else {
        link.close("busy");
        return;
    };
    if slot.as_ref().is_some_and(|pending| pending.outcome.is_none()) {
        // One pairing at a time: a second machine mid-ceremony would put two codes on
        // one screen and the person could confirm the wrong one.
        link.close("another pairing is in progress");
        return;
    }
    let with = link.peer_id().to_string();
    tracing::info!(peer = %with, "a machine wants to pair — the same six digits are showing on both screens");
    let link = Arc::new(link);
    *slot = Some(PendingPairing {
        code: code.to_display_string().replace(' ', ""),
        with,
        link: Arc::clone(&link),
        outcome: None,
    });
    drop(slot);
    // Two minutes of patience: a code nobody confirms is a doorbell nobody answered.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        if let Ok(mut slot) = UI_PAIRING.lock()
            && let Some(pending) = slot.as_ref()
            && pending.outcome.is_none()
            && Arc::ptr_eq(&pending.link, &link)
        {
            pending.link.close("pairing timed out");
            *slot = None;
        }
    });
}

/// The loopback port THIS process's page is on — zero until the page is up.
///
/// Exists so shutdown can tell whether the `ui-port` note on disk is still its own.
/// During a takeover the dying seam and the one replacing it share that file, and
/// deleting it unconditionally erased the survivor's note: the running seam kept
/// serving, but every page and script that looks the port up found nothing. Observed
/// live — the fleet was healthy and unreachable at the same time.
static UI_PORT_SELF: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Remove the `ui-port` note only if it still names this process's own page.
fn remove_ui_port_note_if_ours(dir: &std::path::Path) {
    let own = UI_PORT_SELF.load(std::sync::atomic::Ordering::Relaxed);
    let note = dir.join("ui-port");
    let noted =
        std::fs::read_to_string(&note).ok().and_then(|text| text.trim().parse::<u16>().ok());
    if own != 0 && noted == Some(own) {
        let _ = std::fs::remove_file(note);
    }
}

/// Note what the clipboard is doing, for the UI.
fn note_transfer(what: &str, detail: String) {
    if let Ok(mut slot) = UI_TRANSFER.lock() {
        *slot = Some((what.to_owned(), detail));
    }
}

/// Each peer's seam version, learned from its announcement. A fleet must run matching
/// versions, and until this existed the page could only say "version unknown".
static UI_VERSIONS: std::sync::Mutex<Vec<(seam_proto::PeerId, String)>> =
    std::sync::Mutex::new(Vec::new());

/// What each peer can do — capture and share input, or only replay it. Learned from
/// the peer's own announcement, never assumed.
static UI_ROLES: std::sync::Mutex<Vec<(seam_proto::PeerId, &'static str)>> =
    std::sync::Mutex::new(Vec::new());

/// Peers this machine dialled — it is their client; the ones that dialled us are ours.
static UI_DIALLED: std::sync::Mutex<Vec<seam_proto::PeerId>> = std::sync::Mutex::new(Vec::new());

/// Peers the user has switched off in the UI. Input is not forwarded to a disabled
/// machine and it is not placed in the layout, but the link and its clipboard stay.
static UI_DISABLED: std::sync::Mutex<Vec<seam_proto::PeerId>> = std::sync::Mutex::new(Vec::new());

/// Peer placements (which edge each sits on), for the UI's desk mapping.
static UI_PLACES: std::sync::Mutex<Vec<(seam_proto::PeerId, &'static str)>> =
    std::sync::Mutex::new(Vec::new());

/// Set when the UI asks for input to come home; consumed by the forwarding loop.
static UI_RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Which machine holds the pointer, for the UI. `None` means this one.
static UI_FOCUS: std::sync::Mutex<Option<seam_proto::PeerId>> = std::sync::Mutex::new(None);

/// The design artifacts, embedded verbatim.
///
/// These files are a MIRROR of the Claude Design "Seam Pages" project — the source of
/// truth for everything visible. They are pulled from there, never edited here; see
/// docs/GOAL.md §12a. The daemon serves them so the UI always matches the daemon it
/// talks to, with no separate install.
/// Binary assets — the logo. Kept apart from `UI_PAGES` because those are text and these
/// are not; one table of `&str` cannot hold a PNG.
const UI_ASSETS: &[(&str, &[u8], &str)] = &[
    ("/_ds/assets/logo/seam-logo-32.png", include_bytes!("../ui/_ds/assets/logo/seam-logo-32.png"), "image/png"),
    ("/_ds/assets/logo/seam-logo-128.png", include_bytes!("../ui/_ds/assets/logo/seam-logo-128.png"), "image/png"),
];

const UI_PAGES: &[(&str, &str, &str)] = &[
    ("/", include_str!("../ui/index.html"), "text/html; charset=utf-8"),
    ("/index.html", include_str!("../ui/index.html"), "text/html; charset=utf-8"),
    ("/transfers.html", include_str!("../ui/transfers.html"), "text/html; charset=utf-8"),
    ("/pairing.html", include_str!("../ui/pairing.html"), "text/html; charset=utf-8"),
    ("/settings.html", include_str!("../ui/settings.html"), "text/html; charset=utf-8"),
    ("/chrome.html", include_str!("../ui/chrome.html"), "text/html; charset=utf-8"),
    ("/onboarding.html", include_str!("../ui/onboarding.html"), "text/html; charset=utf-8"),
    ("/doctor.html", include_str!("../ui/doctor.html"), "text/html; charset=utf-8"),
    ("/update.html", include_str!("../ui/update.html"), "text/html; charset=utf-8"),
    ("/ideas.html", include_str!("../ui/ideas.html"), "text/html; charset=utf-8"),
    ("/join.html", include_str!("../ui/join.html"), "text/html; charset=utf-8"),
    ("/layout.html", include_str!("../ui/layout.html"), "text/html; charset=utf-8"),
    (
        "/_ds/page-layout.js",
        include_str!("../ui/_ds/page-layout.js"),
        "text/javascript; charset=utf-8",
    ),
    ("/licence.html", include_str!("../ui/licence.html"), "text/html; charset=utf-8"),
    (
        "/_ds/page-licence.js",
        include_str!("../ui/_ds/page-licence.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "/_ds/page-update.js",
        include_str!("../ui/_ds/page-update.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "/_ds/page-join.js",
        include_str!("../ui/_ds/page-join.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "/_ds/page-pairing.js",
        include_str!("../ui/_ds/page-pairing.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "/notifications.html",
        include_str!("../ui/notifications.html"),
        "text/html; charset=utf-8",
    ),
    ("/_ds/tokens.css", include_str!("../ui/_ds/tokens.css"), "text/css; charset=utf-8"),
    ("/_ds/bind.js", include_str!("../ui/_ds/bind.js"), "text/javascript; charset=utf-8"),
    ("/_ds/shared.js", include_str!("../ui/_ds/shared.js"), "text/javascript; charset=utf-8"),
    ("/_ds/nav.js", include_str!("../ui/_ds/nav.js"), "text/javascript; charset=utf-8"),
    ("/_ds/activity.js", include_str!("../ui/_ds/activity.js"), "text/javascript; charset=utf-8"),
    ("/_ds/quit.js", include_str!("../ui/_ds/quit.js"), "text/javascript; charset=utf-8"),
    ("/join.sh", include_str!("../../../scripts/join.sh"), "text/x-shellscript; charset=utf-8"),
    ("/join.ps1", include_str!("../../../scripts/join.ps1"), "text/plain; charset=utf-8"),
    (
        "/_ds/page-settings.js",
        include_str!("../ui/_ds/page-settings.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "/_ds/page-doctor.js",
        include_str!("../ui/_ds/page-doctor.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "/_ds/page-transfers.js",
        include_str!("../ui/_ds/page-transfers.js"),
        "text/javascript; charset=utf-8",
    ),
];

fn ui_asset_response(request: &str, path: &str, port: u16) -> Option<Vec<u8>> {
    let (_, asset, content_type) = UI_ASSETS.iter().find(|(route, _, _)| *route == path)?;
    let (status, content_type, body): (&str, &str, &[u8]) = if request_is_local(request, port) {
        ("200 OK", content_type, asset)
    } else {
        ("403 Forbidden", "text/plain", b"refused")
    };
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len(),
    )
    .into_bytes();
    response.extend_from_slice(body);
    Some(response)
}

/// Serve the fleet UI on a loopback port, and record the port for `seam ui`.
///
/// Loopback only, read-only, no external requests possible. Hand-rolled GET handling
/// rather than a web framework: nine static routes and one JSON endpoint do not justify
/// a dependency tree on the input path's binary.
fn start_ui_server(
    dir: std::path::PathBuf,
    name: String,
    id: seam_proto::PeerId,
    seam_port: u16,
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    store: SharedTrust,
    endpoint: Option<Arc<seam_transport::Endpoint>>,
) {
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(e) => {
                tracing::warn!("no ui: could not bind a loopback port: {e}");
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(e) => {
                tracing::warn!("no ui: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(dir.join("ui-port"), port.to_string()) {
            tracing::warn!("ui is up but 'seam ui' will not find it: {e}");
        }
        UI_PORT_SELF.store(port, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(port, "fleet page ready — 'seam ui' opens it");

        let ui_port_note = dir.join("ui-port");
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { continue };
            let links = Arc::clone(&links);
            let name = name.clone();
            let ui_port_note = ui_port_note.clone();
            let store = Arc::clone(&store);
            let endpoint = endpoint.clone();
            let dir = dir.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // Read until the headers end, not just once: a single read is not
                // guaranteed to contain them, and the Origin check below is only sound
                // if the header is actually present in what was parsed.
                let mut buf = Vec::with_capacity(2048);
                let mut chunk = [0u8; 2048];
                loop {
                    let Ok(n) = socket.read(&mut chunk).await else { return };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                if let Some(response) = ui_asset_response(&request, path, port) {
                    let _ = socket.write_all(&response).await;
                    return;
                }
                let (status, ctype, body) =
                    route(
                        &request,
                        path,
                        &Serving {
                            ui_port: port,
                            seam_port,
                            name: &name,
                            id,
                            links: &links,
                            ui_port_note: &ui_port_note,
                            store: &store,
                            endpoint: endpoint.as_ref(),
                            dir: &dir,
                        },
                    )
                    .await;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                     Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
}

/// Is this request really from the fleet page on this machine?
///
/// A browser on any website can POST to a loopback port — a simple POST needs no
/// preflight, so `fetch('http://127.0.0.1:PORT/action/quit')` from a random page would
/// have killed this daemon, disabled peers or released input. Two checks close that:
///
/// - **Origin**: browsers always send it on a cross-origin POST. Anything but our own
///   origin is refused.
/// - **Host**: defeats DNS rebinding, where an attacker's name resolves to 127.0.0.1 so
///   the request looks local but the page driving it is not.
///
/// Non-browser callers (curl, scripts) send no Origin and are allowed: they already run
/// as the user and gain nothing they did not have.
fn request_is_local(request: &str, port: u16) -> bool {
    let header = |name: &str| -> Option<&str> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    };
    let origin_ok = header("origin").is_none_or(|origin| {
        origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}")
    });
    let host_ok = header("host")
        .is_none_or(|host| host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}"));
    origin_ok && host_ok
}

#[cfg(test)]
mod ui_origin {
    use super::{
        UI_ASSETS, UI_PAGES, request_is_local, serve_activation_only, start_ui_server,
        ui_asset_response,
    };
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    fn get(headers: &str) -> String {
        format!("POST /action/quit HTTP/1.1\r\n{headers}\r\n\r\n")
    }

    #[test]
    fn a_page_on_another_site_cannot_drive_this_daemon() {
        // The attack: any website the user visits POSTs to the loopback port. A simple
        // POST is not preflighted, so it lands unless the origin is checked.
        assert!(!request_is_local(
            &get("Host: 127.0.0.1:5000\r\nOrigin: https://evil.example"),
            5000
        ));
    }

    #[test]
    fn dns_rebinding_cannot_dress_up_as_local() {
        assert!(!request_is_local(&get("Host: evil.example"), 5000));
    }

    #[test]
    fn the_fleet_page_itself_is_allowed() {
        assert!(request_is_local(
            &get("Host: 127.0.0.1:5000\r\nOrigin: http://127.0.0.1:5000"),
            5000
        ));
        assert!(request_is_local(&get("Host: localhost:5000"), 5000));
    }

    #[test]
    fn a_non_browser_caller_is_allowed() {
        // curl and scripts send no Origin; they already run as the user.
        assert!(request_is_local(&get("Host: 127.0.0.1:5000"), 5000));
    }

    #[test]
    fn every_embedded_logo_is_a_real_png() {
        assert_eq!(UI_ASSETS.len(), 2);
        for (path, bytes, content_type) in UI_ASSETS {
            assert!(
                std::path::Path::new(path)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
            );
            assert_eq!(*content_type, "image/png");
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert!(bytes.len() > 100, "{path} is unexpectedly small");
        }
    }

    #[test]
    fn an_embedded_logo_response_preserves_the_exact_png_bytes() {
        let path = UI_ASSETS[0].0;
        let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:5000\r\n\r\n");
        let response = ui_asset_response(&request, path, 5000).expect("known logo route");
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response has a header terminator");
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n"));
        assert_eq!(&response[split + 4..], UI_ASSETS[0].1);
    }

    #[test]
    fn an_embedded_logo_refuses_a_foreign_origin() {
        let path = UI_ASSETS[0].0;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:5000\r\nOrigin: https://evil.example\r\n\r\n"
        );
        let response = ui_asset_response(&request, path, 5000).expect("known logo route");
        assert!(response.starts_with(b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\n"));
        assert!(response.ends_with(b"\r\n\r\nrefused"));
    }

    #[test]
    fn a_non_asset_path_stays_with_the_text_router() {
        assert!(ui_asset_response("GET / HTTP/1.1\r\n\r\n", "/", 5000).is_none());
    }

    async fn wait_for_ui_port(dir: &std::path::Path) -> u16 {
        for _ in 0..100 {
            if let Ok(port) = std::fs::read_to_string(dir.join("ui-port"))
                && let Ok(port) = port.trim().parse()
            {
                return port;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("UI server did not publish its port");
    }

    async fn fetch(port: u16, path: &str) -> Vec<u8> {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("test UI server accepts a loopback connection");
        socket
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
            .await
            .expect("request writes");
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.expect("response reads");
        response
    }

    #[tokio::test]
    async fn fleet_server_serves_the_embedded_logo() {
        let dir = std::env::temp_dir().join(format!("seam-logo-fleet-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test state directory");
        start_ui_server(
            dir.clone(),
            "test".to_owned(),
            seam_proto::PeerId([1; 16]),
            24810,
            Arc::new(tokio::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::RwLock::new(seam_transport::TrustStore::new())),
            None,
        );
        let port = wait_for_ui_port(&dir).await;
        let response = fetch(port, UI_ASSETS[0].0).await;
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n"));
        assert!(response.ends_with(UI_ASSETS[0].1));
        std::fs::remove_dir_all(dir).expect("test state directory removes");
    }

    #[tokio::test]
    async fn activation_server_serves_the_embedded_logo() {
        let dir =
            std::env::temp_dir().join(format!("seam-logo-activation-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test state directory");
        let server_dir = dir.clone();
        let server = tokio::spawn(async move { serve_activation_only(&server_dir, false).await });
        let port = wait_for_ui_port(&dir).await;
        let response = fetch(port, UI_ASSETS[1].0).await;
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n"));
        assert!(response.ends_with(UI_ASSETS[1].1));
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(dir).expect("test state directory removes");
    }

    #[test]
    fn every_html_page_uses_the_embedded_favicon() {
        let pages: Vec<_> = UI_PAGES
            .iter()
            .filter(|(_, _, content_type)| content_type.starts_with("text/html"))
            .collect();
        assert!(!pages.is_empty());
        for (path, html, _) in pages {
            assert!(
                html.contains(r#"href="_ds/assets/logo/seam-logo-32.png""#),
                "{path} has no seam favicon"
            );
        }
    }

    #[test]
    fn primary_brand_surfaces_use_the_full_lockup() {
        for path in ["/", "/index.html", "/onboarding.html", "/licence.html"] {
            let (_, html, _) = UI_PAGES
                .iter()
                .find(|(route, _, _)| *route == path)
                .expect("primary brand page must be embedded");
            assert!(html.contains(r#"class="brand-lockup""#), "{path}");
            assert!(html.contains("seam-logo-128.png"), "{path}");
        }
    }
}

/// The last few things that actually happened, read from the log seam already writes.
///
/// The fleet page previously showed invented timestamps and events — a design mock-up
/// serving as product UI, which is a lie told in the user's own interface. This reads the
/// real log tail instead, and shows nothing when there is nothing.
fn recent_activity(dir: &std::path::Path) -> String {
    use std::fmt::Write as _;
    // Only lines a person would care about; the log also carries per-event noise.
    const INTERESTING: [&str; 10] = [
        "moved to this peer",
        "back on this machine",
        "clipboard received",
        "clipboard image received",
        "clipboard files received",
        "clipboard changed; sharing",
        "found and connected",
        "peer went away",
        "seam is running",
        "peer enabled",
    ];
    let Ok(text) = std::fs::read_to_string(dir.join("seam.log")) else {
        return String::new();
    };
    let mut out = String::new();
    let lines: Vec<&str> = text.lines().rev().take(400).collect();
    for line in lines {
        if out.matches('{').count() >= 8 {
            break;
        }
        let plain = strip_ansi(line);
        let Some((stamp, message)) = plain.split_once(" INFO ") else {
            continue;
        };
        if !INTERESTING.iter().any(|needle| message.contains(needle)) {
            continue;
        }
        // "2026-07-25T18:31:21.521281Z" -> "18:31:21"
        let time = stamp.trim().split('T').nth(1).map_or("", |t| t.get(0..8).unwrap_or(""));
        let text = message.trim().replace('"', "'");
        if !out.is_empty() {
            out.push(',');
        }
        let _ = write!(out, r#"{{"time":"{time}","what":"{text}"}}"#);
    }
    out
}

/// Drop ANSI colour codes the log writer adds for terminals.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for skip in chars.by_ref() {
                if skip == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// What the last check of the downloads page found.
static UI_UPDATE: std::sync::Mutex<Option<(String, String, String)>> =
    std::sync::Mutex::new(None);

/// Show a native notification. Best effort and deliberately silent on failure: a machine
/// that cannot post a notification must still run seam.
fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        // osascript rather than a notification crate: no dependency, and it degrades to
        // nothing on a machine where notifications are refused.
        let script = format!(
            "display notification {} with title {}",
            applescript_string(body),
            applescript_string(title)
        );
        let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, \
             ContentType=WindowsRuntime] | Out-Null; \
             $t=[Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent(0); \
             $t.GetElementsByTagName('text').Item(0).AppendChild($t.CreateTextNode('{}'))|Out-Null; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('seam')\
             .Show([Windows.UI.Notifications.ToastNotification]::new($t))",
            format!("{title} — {body}").replace('\'', "")
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (title, body);
    }
}

/// Quote a string for `AppleScript`, where the only escape that matters is the quote itself.
#[cfg(target_os = "macos")]
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace(['\\', '"'], ""))
}

/// Poll the public releases repo so the update page can react rather than be read.
///
/// The daemon does the looking, not the page: a browser cannot reach the releases API
/// from a loopback page without exposing that page to the network, and the daemon is
/// already running. Hourly is deliberate — a release is not urgent, and a background
/// program hammering an API is a bad neighbour.
fn start_update_watch() {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            let Ok(output) = tokio::process::Command::new("curl")
                .args([
                    "-fsSL",
                    "--max-time",
                    "10",
                    "-H",
                    "Accept: application/vnd.github+json",
                    "https://api.github.com/repos/amor-openmind/seam-releases/releases/latest",
                ])
                .output()
                .await
            else {
                continue;
            };
            let body = String::from_utf8_lossy(&output.stdout);
            // Two fields, so a hand-rolled read rather than a JSON dependency for this.
            // GitHub pretty-prints its JSON, so the colon is followed by a space:
            // `"tag_name": "v0.7.1"`. Matching `"tag_name":"` found nothing and the check
            // silently never succeeded, which the page reported for hours as "not checked
            // yet" — a parser that fails closed and says nothing is worse than one that
            // errors.
            let field = |key: &str| -> Option<String> {
                let at = body.find(&format!("\"{key}\""))?;
                let rest = &body[at..];
                let colon = rest.find(':')?;
                let open = rest[colon..].find('"')? + colon + 1;
                let end = rest[open..].find('"')? + open;
                Some(rest[open..end].to_owned())
            };
            let Some(tag) = field("tag_name") else { continue };
            let latest = tag.trim_start_matches('v').to_owned();
            let published = field("published_at").unwrap_or_default();
            // Say it out loud, once per version. A page nobody has open cannot tell
            // anyone anything, and a fleet that must run matching versions needs the
            // machine to speak up rather than wait to be visited.
            let is_new = latest != env!("CARGO_PKG_VERSION");
            let already_told = UI_UPDATE
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .is_some_and(|(seen, _, _)| seen == latest);
            if is_new && !already_told {
                tracing::warn!(
                    latest = %latest,
                    running = env!("CARGO_PKG_VERSION"),
                    "a newer seam is available — every machine must run the same version"
                );
                notify(&format!("seam {latest} is available"), "Every machine must run the same version.");
            }
            if let Ok(mut slot) = UI_UPDATE.lock() {
                *slot = Some((latest, published, tag));
            }
        }
    });
}

/// This machine's address on the network, as another machine would reach it.
///
/// Asked of the routing table via a connectionless UDP socket — no packet is sent. The
/// join command previously used the browser's view, which on a loopback page is
/// `127.0.0.1`: meaningless on the machine being added, and the cause of a join command
/// that could never work.
fn lan_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

/// The arrangement a person set, if they set one.
///
/// Pairing order is a guess: first paired sits left, second below. It is right often
/// enough to be useful and wrong in a way nothing could detect — a screen's physical
/// position is the one fact about a desk that is not visible from software. When someone
/// says where a machine actually is, that answer outranks the guess and survives restarts.
/// Where machine-wide state lives, whatever folder seam was started from.
///
/// The same reasoning as the licence: things that describe the MACHINE — its licence, the
/// shape of its desk — belong somewhere a reinstall cannot move. `SEAM_HOME` still
/// overrides everything, for testing.
fn layout_home() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("SEAM_HOME")
        && !home.is_empty()
    {
        return Some(std::path::PathBuf::from(home));
    }
    let dir = directories::ProjectDirs::from("dev", "seam", "seam")?.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn stored_layout(dir: &std::path::Path) -> Vec<(String, String, String)> {
    // Machine-wide first, then beside the install. How your screens are arranged is a fact
    // about the desk, not about which folder the binary happens to sit in — storing it
    // beside the install meant a reinstall to a new folder silently lost the arrangement,
    // exactly as it once lost the licence.
    let text = layout_home()
        .and_then(|home| std::fs::read_to_string(home.join("layout")).ok())
        .or_else(|| std::fs::read_to_string(dir.join("layout")).ok());
    let Some(text) = text else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.trim().split(' ');
            let peer = parts.next()?;
            let edge = parts.next()?;
            // A missing anchor means "next to this machine", which is what every
            // arrangement written before relations existed meant.
            let anchor = parts.next().unwrap_or("self");
            matches!(edge, "left" | "right" | "top" | "bottom").then(|| {
                (peer.to_owned(), edge.to_owned(), anchor.to_owned())
            })
        })
        .collect()
}

/// Record that a machine sits on one side of another, replacing any earlier answer.
///
/// A relation, not a side of this machine. A desk is a chain: a laptop below the iMac,
/// which is itself left of this machine. Describing that as "below this machine" is simply
/// false, and it sent the pointer through an edge nothing was on.
fn set_layout(dir: &std::path::Path, short_id: &str, edge: &str, anchor: &str) -> bool {
    if !matches!(edge, "left" | "right" | "top" | "bottom") || short_id.is_empty() {
        return false;
    }
    // A machine cannot sit beside itself, and a pair cannot each be beside the other:
    // either would be a layout with no fixed point to draw from.
    if anchor == short_id {
        return false;
    }
    let mut lines: Vec<String> = stored_layout(dir)
        .into_iter()
        .filter(|(peer, _, _)| peer != short_id)
        .map(|(peer, e, a)| format!("{peer} {e} {a}"))
        .collect();
    lines.push(format!("{short_id} {edge} {anchor}"));
    let body = lines.join("\n");
    // Written machine-wide so it survives a reinstall anywhere, and beside the install too
    // so a portable copy carried to another machine takes the desk with it.
    if let Some(home) = layout_home() {
        let _ = std::fs::write(home.join("layout"), &body);
    }
    let written = std::fs::write(dir.join("layout"), &body).is_ok();
    if written {
        tracing::info!(peer = short_id, edge, anchor, "arrangement set from the desk page");
        UI_RELAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    written
}

/// Set when the arrangement changed and the layout must be rebuilt.
static UI_RELAYOUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Was this the first machine seam was installed on?
///
/// Decided once, on first run, by whether anything had been paired yet — then written
/// down. A machine that starts alone is the one a person is sitting at, and every machine
/// added afterwards joins it; deriving this fresh each time would let it flip whenever
/// connections raced.
fn first_machine(dir: &std::path::Path) -> bool {
    if dir.join("joined").exists() {
        return false;
    }
    // Anything else is the machine seam was installed on. Having paired peers does NOT
    // make a machine a client — the original server has more pairings than anyone, and
    // reading them as evidence of joining relabelled it as a client on its own fleet.
    // Only dialling out marks a machine as joined, and `daemon` writes that when it does.
    if !dir.join("first-machine").exists() {
        let _ = std::fs::write(dir.join("first-machine"), "");
    }
    true
}

/// The licence, as the page sees it: who it is for, and when it stops.
fn licence_json(dir: &std::path::Path) -> String {
    licence::stored(dir).map_or_else(
        || "null".to_owned(),
        |l| format!(r#"{{"name":"{}","expires":{}}}"#, l.name.replace('"', "'"), l.expires_day),
    )
}

/// What the last check of the downloads page found, with the links a person would follow.
fn update_json() -> String {
    UI_UPDATE.lock().ok().and_then(|slot| slot.clone()).map_or_else(
        || "null".to_owned(),
        |(latest, published, tag)| {
            const RELEASES: &str = "https://github.com/amor-openmind/seam-releases/releases";
            let asset = if cfg!(target_os = "windows") { "seam.exe" } else { "seam-macos-arm64" };
            format!(
                r#"{{"latest":"{latest}","published":"{}","page":"{RELEASES}/tag/{tag}","asset":"{RELEASES}/download/{tag}/{asset}","checked":"just now"}}"#,
                published.split('T').next().unwrap_or(""),
            )
        },
    )
}

/// Everything the page needs to say about one peer.
///
/// Split out of `ui_state_json` for length, but the grouping is real: each of these is
/// derived, none configured, and the arrangement ones come from what a person SET rather
/// than from what the graph currently managed to place.
fn peer_facts(
    dir: &std::path::Path,
    peer: seam_proto::PeerId,
) -> (String, String, &'static str, bool) {
    let chosen = stored_layout(dir)
        .into_iter()
        .find(|(p, _, _)| peer.to_string().starts_with(p.as_str()));

    // A machine that could not be placed yet — because its neighbour was still asleep —
    // reported no edge at all, and the page drew it somewhere default. From a chair that
    // is the desk rearranging itself after a laptop sleeps, when the arrangement on disk
    // was right the whole time. Placement is a consequence; what was set is the fact.
    let edge = chosen.as_ref().map_or_else(
        || {
            UI_PLACES
                .lock()
                .ok()
                .and_then(|places| {
                    places.iter().find(|(p, _)| *p == peer).map(|(_, e)| (*e).to_owned())
                })
                .unwrap_or_default()
        },
        |(_, e, _)| e.clone(),
    );
    let anchor = chosen.map_or_else(|| "self".to_owned(), |(_, _, a)| a);

    // Capability, learned by observation: a peer that has sent motion captures input.
    let role = UI_ROLES
        .lock()
        .ok()
        .and_then(|roles| roles.iter().find(|(p, _)| *p == peer).map(|(_, r)| *r))
        .unwrap_or("receives input");
    let enabled = UI_DISABLED.lock().is_ok_and(|off| !off.contains(&peer));

    (edge, anchor, role, enabled)
}

/// The live state the UI binds to. Assembled by hand — ten fields do not justify serde.
async fn ui_state_json(
    name: &str,
    id: seam_proto::PeerId,
    ui_port: u16,
    seam_port: u16,
    dir: &std::path::Path,
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
) -> String {
    use std::fmt::Write as _;

    let focus = UI_FOCUS
        .lock()
        .ok()
        .and_then(|guard| *guard)
        .map_or_else(|| "local".to_owned(), |peer| peer.to_string());

    let mut peers = String::new();
    for link in links.lock().await.iter() {
        if !peers.is_empty() {
            peers.push(',');
        }
        let peer = link.peer_id();
        let (edge, anchor, role, enabled) = peer_facts(dir, peer);
        let _ = write!(
            peers,
            r#"{{"id":"{peer}","name":"{peer}","addr":"{}","edge":"{edge}","anchor":"{anchor}","role":"{role}","enabled":{enabled},"version":"{}","rtt_ms":{}}}"#,
            link.remote_address(),
            UI_VERSIONS
                .lock()
                .ok()
                .and_then(|v| v.iter().find(|(p, _)| *p == peer).map(|(_, ver)| ver.clone()))
                .unwrap_or_default(),
            // The QUIC smoothed round-trip: a continuous, in-band measurement of the
            // exact path input rides. "Everything pauses for seconds then works" is
            // either this number spiking (the network breathing) or this number flat
            // while the far machine chokes — one glance separates the two, where ping
            // cannot (ICMP is firewalled off on the Windows machines).
            link.rtt().as_millis()
        );
    }

    // This machine's own capability, decided by what the build can actually do rather
    // than by any negotiation. Capture exists on macOS today; Windows replays only.
    // Server or client is decided by how this machine entered the fleet, and is known
    // from the first run rather than negotiated: the machine seam was installed on is the
    // server, and a machine that ran the join command is a client. Deriving it from
    // connections would let it change under a race; deriving it from capability made two
    // idle machines both claim to be servers.
    //
    // Capability still qualifies it. A build that cannot capture input says so, because a
    // role promising something it cannot do would be a label that lies.
    let self_role = if !cfg!(target_os = "macos") {
        "receives input"
    } else if first_machine(dir) {
        "shares input"
    } else {
        "receives input, joined this fleet"
    };

    let shares = UI_SHARES.lock().map_or((true, true, true), |s| *s);

    let mut health = String::new();
    let mut push_health = |ok: bool, text: &str| {
        if !health.is_empty() {
            health.push(',');
        }
        let _ = write!(health, r#"{{"ok":{ok},"text":"{text}"}}"#);
    };
    if let Some(report) = seam_input::permission_report() {
        for (_, granted, _) in report {
            push_health(granted, if granted { "granted" } else { "missing — see doctor" });
        }
    }
    #[cfg(target_os = "macos")]
    {
        push_health(true, "window server granted");
        push_health(
            seam_input::macos::capture_is_alive() != Some(false),
            "alive",
        );
    }
    #[cfg(target_os = "windows")]
    {
        let elevated = seam_input::windows::is_elevated();
        push_health(elevated, if elevated { "elevated" } else { "not elevated — see doctor" });
    }

    let (discovered, pairing) = pairing_state_json();

    format!(
        r#"{{"version":"{}","name":"{}","id":"{id}","platform":"{}/{}","role":"{}","port":{},"seamPort":{},"lan":"{}","focus":"{focus}","transfer":{},"shares":{{"text":{},"images":{},"files":{}}},"startup":{},"licence":{},"update":{},"activity":[{}],"peers":[{peers}],"health":[{health}],"discovered":[{discovered}],"pairing":{pairing}}}"#,
        env!("CARGO_PKG_VERSION"),
        name.replace('"', "'"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        // Role is per-pair and derived: the machine that ACCEPTED a connection is the
        // server of that pair. This machine is therefore a server whenever at least one
        // peer dialled it — i.e. some connected peer is not in the dialled list. Reading
        // the empty dialled list as "server" was backwards: it made a machine that had
        // dialled nothing (because nothing was connected at all) claim to be a server,
        // which is why two idle machines both said "server".
        self_role,
        ui_port,
        seam_port,
        lan_address().unwrap_or_default(),
        UI_TRANSFER.lock().ok().and_then(|slot| slot.clone()).map_or_else(
            || "null".to_owned(),
            |(what, detail)| format!(r#"{{"what":"{what}","detail":"{detail}"}}"#),
        ),
        shares.0,
        shares.1,
        shares.2,
        starts_at_login(),
        licence_json(dir),
        update_json(),
        recent_activity(dir),
    )
}

/// Accept inbound links: members join the fleet, strangers ring the pairing doorbell.
fn start_accepting(
    endpoint: Arc<seam_transport::Endpoint>,
    store: SharedTrust,
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    geometry: Geometry,
    clipboard: Clipboard,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            match incoming {
                Ok(link) => {
                    let peer = link.peer_id();
                    let verdict = store
                        .read()
                        .map_or(Err(seam_transport::Error::NotPaired), |s| link.authorize(&s));
                    match verdict {
                        Ok(()) => {
                            tracing::info!(%peer, remote = %link.remote_address(), "peer connected");
                            let link = Arc::new(link);
                            register_link(&links, &link, &clipboard).await;
                            announce_geometry(&link).await;
                            tokio::spawn(receive_from(
                                link,
                                Arc::clone(&geometry),
                                Arc::clone(&clipboard),
                                Arc::clone(&links),
                            ));
                        }
                        Err(seam_transport::Error::NotPaired) => {
                            // A stranger is not an error, it is a doorbell: hold the
                            // link and put six digits on both screens (goal §14).
                            park_pairing(link);
                        }
                        Err(e) => {
                            // Never silent: a refused peer says why (goal O5).
                            tracing::warn!(%peer, "refused: {e}");
                            link.close("not paired");
                        }
                    }
                }
                Err(e) => tracing::warn!("an inbound connection failed: {e}"),
            }
        }
    })
}

/// Inject a remote button press, with the two witnesses that end silent failures:
/// the held-modifier check before, and the pressed-state read-back after.
fn apply_button(peer: seam_proto::PeerId, button: u8, down: bool) {
    if down {
        warn_if_modifiers_held();
    }
    if let Err(e) = seam_input::inject_button(button, down) {
        tracing::warn!(%peer, "could not press a mouse button: {e}");
    }
    #[cfg(target_os = "windows")]
    if down && !seam_input::windows::button_reads_pressed(button) {
        tracing::warn!(
            %peer,
            button,
            "a click was injected and accepted, yet the system does not show the \
             button as pressed — something between seam and the desktop is \
             discarding clicks"
        );
    }
}

/// Name any modifier held on this machine that seam did not press — throttled.
///
/// Runs when a click arrives, because that is the moment a phantom modifier turns the
/// click into a chord: "clicks and keyboard not working" on a machine whose elevation,
/// links and injection all checked out was a held key nobody could see. Windows only;
/// elsewhere this is a no-op.
fn warn_if_modifiers_held() {
    #[cfg(target_os = "windows")]
    {
        static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
        let due = LAST
            .lock()
            .ok()
            .is_some_and(|slot| slot.is_none_or(|at| at.elapsed() > Duration::from_secs(5)));
        if !due {
            return;
        }
        let held = seam_input::windows::oddly_held_modifiers();
        if held.is_empty() {
            return;
        }
        if let Ok(mut slot) = LAST.lock() {
            *slot = Some(std::time::Instant::now());
        }
        tracing::warn!(
            held = held.join(", "),
            "a modifier seam did not press is held on this machine — clicks land as \
             chords and keys as shortcuts; press and release it on this machine's own \
             keyboard"
        );
    }
}

/// Undo whatever a previous seam left behind, and armour the console.
///
/// Crash recovery: a seam that died while the cursor was concealed leaves this machine
/// with an invisible cursor, and one that died mid-keystroke leaves phantom held keys —
/// both restores are free when nothing was left behind. `QuickEdit` goes off so a stray
/// click in our own console can never freeze the daemon at a log line again.
#[cfg(target_os = "windows")]
fn recover_this_machine() {
    seam_input::windows::disable_console_quick_edit();
    seam_input::windows::reveal_cursor();
    seam_input::windows::release_stuck_modifiers();
    repair_start_at_login_path();
    sweep_fossil_binaries();
}

/// Delete versioned seam binaries left beside this one by an early installer.
///
/// The install folder held `seam-0.6.2.exe` fossils from the era before the installer
/// settled on one filename. They are complete, runnable, ancient seams — and one of
/// them, woken by a stray double-click, held the fleet's port for an afternoon while
/// speaking a protocol two generations old. A fossil that does not exist cannot be
/// resurrected. Best effort: a fossil that is RUNNING holds a lock and stays; the
/// takeover's wildcard kill handles that one.
#[cfg(target_os = "windows")]
fn sweep_fossil_binaries() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().to_lowercase();
        if name.starts_with("seam-")
            && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            && std::fs::remove_file(&path).is_ok()
        {
            tracing::info!(%name, "removed a versioned seam binary left by an older installer");
        }
    }
}

/// Point start-at-login at the seam that is actually running.
///
/// The Run key records the exe path that was current when the toggle was written — and
/// then outlives it. A laptop's key still said `Downloads\seam.exe` from version 0.4.2,
/// so every sign-in resurrected a months-old relic that fought the real seam for the
/// port all morning: handshakes failing with "authentication failed", links resetting
/// as the port changed hands. The running seam is the truth; the key follows it.
#[cfg(target_os = "windows")]
fn repair_start_at_login_path() {
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let Ok(exe) = std::env::current_exe() else { return };
    if starts_at_login() && seam_input::windows::is_elevated() {
        // Re-record unconditionally: the logon task follows the exe that is actually
        // running, and a leftover Run key gets migrated to a highest-privileges task —
        // which is what stops sign-ins producing an unelevated seam whose pointer dies
        // on every administrator window.
        let _ = set_start_at_login(true);
        return;
    }
    let Ok(out) =
        std::process::Command::new("reg").args(["query", RUN_KEY, "/v", "seam"]).output()
    else {
        return;
    };
    if !out.status.success() {
        return; // Start-at-login is off; nothing to repair.
    }
    let recorded = String::from_utf8_lossy(&out.stdout);
    let wanted = format!("\"{}\" run --no-ui", exe.to_string_lossy());
    if recorded.contains(&*exe.to_string_lossy()) {
        return; // Already points at this seam.
    }
    let ok = std::process::Command::new("reg")
        .args(["add", RUN_KEY, "/v", "seam", "/t", "REG_SZ", "/d", &wanted, "/f"])
        .output()
        .is_ok_and(|o| o.status.success());
    if ok {
        tracing::info!(
            "start-at-login pointed at an older seam; it now starts this one"
        );
    }
}

/// Bind the endpoint, or — if seam is already running here — show that one instead.
///
/// Launching twice is what a person does by double-clicking the icon again. Failing with
/// a port message would be technically correct and useless; showing them the seam they
/// already have is the answer. Returns `Err` only for genuine failures.
fn bind_or_show_running(
    dir: &std::path::Path,
    identity: &Arc<Identity>,
    port: u16,
) -> Result<std::ops::ControlFlow<(), Arc<seam_transport::Endpoint>>> {
    match Endpoint::bind(Arc::clone(identity), format!("0.0.0.0:{port}").parse()?) {
        Ok(endpoint) => Ok(std::ops::ControlFlow::Continue(Arc::new(endpoint))),
        Err(e) if is_address_in_use(&e) => {
            // A newly launched seam takes over. Two daemons on one machine is never what
            // anyone wanted - the second used to open the first one's page, which quietly
            // meant a fresh download appeared to do nothing. Ask the running one to stop,
            // then take the port. Its pages close themselves when it goes.
            if stop_running_instance(dir) {
                tracing::info!("stopped the seam that was already running; taking over");
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(endpoint) =
                        Endpoint::bind(Arc::clone(identity), format!("0.0.0.0:{port}").parse()?)
                    {
                        return Ok(std::ops::ControlFlow::Continue(Arc::new(endpoint)));
                    }
                }
            }
            // The polite quit did not free the port. A seam old enough predates the quit
            // endpoint entirely, and it must not win by seniority — the seam a person
            // just started is the one they mean. A stale autostart resurrected a
            // version-0.4.2 relic every sign-in, and the fresh seam bowed out to it.
            #[cfg(target_os = "windows")]
            {
                let own = std::process::id().to_string();
                // Wildcard, not the exact name: an early installer saved VERSIONED
                // binaries, and a resurrected seam-0.6.2.exe held the port for an
                // afternoon while a kill aimed at "seam.exe" sailed right past it.
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/IM", "seam*", "/FI", &format!("PID ne {own}")])
                    .output();
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(endpoint) =
                        Endpoint::bind(Arc::clone(identity), format!("0.0.0.0:{port}").parse()?)
                    {
                        tracing::info!(
                            "an older seam would not quit politely; it was stopped, this one runs"
                        );
                        return Ok(std::ops::ControlFlow::Continue(Arc::new(endpoint)));
                    }
                }
            }
            tracing::info!("seam is already running on this machine; opening its page");
            // If the note is missing the daemon is still running — say that, rather than
            // open_ui's "seam is not running", which would be flatly untrue.
            open_ui(dir).map_err(|_| {
                anyhow::anyhow!(
                    "seam is already running on this machine, but its page address could \
                     not be found. Quit it from its own window, or stop it and start again."
                )
            })?;
            Ok(std::ops::ControlFlow::Break(()))
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------- start at login

/// Does seam start itself when this user signs in?
///
/// Each OS answers this its own way and neither needs a daemon or an installer:
/// macOS reads a `LaunchAgent` plist in `~/Library/LaunchAgents`, Windows a value under
/// `HKCU\...\Run`. Both are per-user, both are removable by hand, and neither requires
/// administrator rights — which matters, because asking for admin to arrange a
/// convenience would be a worse trade than not having it.
#[must_use]
fn starts_at_login() -> bool {
    #[cfg(target_os = "macos")]
    {
        launch_agent_path().is_some_and(|path| path.exists())
    }
    #[cfg(target_os = "windows")]
    {
        // Either mechanism counts: the scheduled task is the elevated one, the Run key
        // the fallback a non-elevated seam can still write.
        let task = std::process::Command::new("schtasks")
            .args(["/query", "/tn", "seam"])
            .output()
            .is_ok_and(|out| out.status.success());
        task || std::process::Command::new("reg")
            .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "seam"])
            .output()
            .is_ok_and(|out| out.status.success())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join("Library/LaunchAgents/dev.seam.seam.plist"))
}

/// Turn start-at-login on or off. Returns what the state actually is afterwards, so the
/// UI reports the truth rather than what was asked for.
fn set_start_at_login(enable: bool) -> bool {
    let Ok(exe) = std::env::current_exe() else { return starts_at_login() };

    #[cfg(target_os = "macos")]
    {
        let Some(path) = launch_agent_path() else { return false };
        if !enable {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &path.to_string_lossy()])
                .output();
            let _ = std::fs::remove_file(&path);
            return starts_at_login();
        }
        // RunAtLoad only; no KeepAlive. A crash loop that relaunches itself forever is
        // worse than seam being off, and the watchdog already covers the case that
        // matters (input left withheld).
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.seam.seam</string>
  <key>ProgramArguments</key><array><string>{}</string><string>run</string><string>--no-ui</string></array>
  <key>RunAtLoad</key><true/>
</dict></plist>
"#,
            exe.to_string_lossy()
        );
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&path, plist).is_err() {
            return false;
        }
        let _ = std::process::Command::new("launchctl")
            .args(["load", &path.to_string_lossy()])
            .output();
        starts_at_login()
    }
    #[cfg(target_os = "windows")]
    {
        const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        if enable {
            // An elevated seam records itself as a highest-privileges logon task, so
            // every sign-in starts it already elevated — no UAC prompt at the door and
            // no pointer silently dying on admin windows (UIPI). A Run key cannot do
            // that: entries there start unelevated, self-elevation then means a UAC
            // prompt at every sign-in, and a declined prompt means input that fails
            // exactly when an administrator window has focus.
            if seam_input::windows::is_elevated() {
                let ok = std::process::Command::new("schtasks")
                    .args([
                        "/create",
                        "/tn",
                        "seam",
                        "/tr",
                        &format!("\"{}\" run --no-ui", exe.to_string_lossy()),
                        "/sc",
                        "onlogon",
                        "/rl",
                        "highest",
                        "/f",
                    ])
                    .output()
                    .is_ok_and(|out| out.status.success());
                if ok {
                    // The task supersedes the key; leaving both starts seam twice.
                    let _ = std::process::Command::new("reg")
                        .args(["delete", RUN_KEY, "/v", "seam", "/f"])
                        .output();
                    return starts_at_login();
                }
            }
            let _ = std::process::Command::new("reg")
                .args([
                    "add",
                    RUN_KEY,
                    "/v",
                    "seam",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &format!("\"{}\" run --no-ui", exe.to_string_lossy()),
                    "/f",
                ])
                .output();
        } else {
            let _ = std::process::Command::new("schtasks")
                .args(["/delete", "/tn", "seam", "/f"])
                .output();
            let _ = std::process::Command::new("reg")
                .args(["delete", RUN_KEY, "/v", "seam", "/f"])
                .output();
        }
        starts_at_login()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (enable, exe);
        false
    }
}

/// Serve the activation page, and nothing else, until this machine has a licence.
///
/// Deliberately minimal: a loopback page and the activate endpoint. No input tap, no QUIC
/// socket, no discovery. An unlicensed seam is a form to fill in, not a running KVM — and
/// it polls its own licence file so the moment activation succeeds it hands over to a real
/// start without anyone typing a command.
async fn serve_activation_only(dir: &std::path::Path, open_page: bool) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    std::fs::write(dir.join("ui-port"), port.to_string()).ok();
    UI_PORT_SELF.store(port, std::sync::atomic::Ordering::Relaxed);
    println!("\n  seam needs a licence on this machine.");
    println!("  Activate it at http://127.0.0.1:{port}/licence.html\n");

    if open_page {
        let url = format!("http://127.0.0.1:{port}/licence.html");
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    }

    let home = dir.to_path_buf();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((mut socket, _)) = accepted else { continue };
                let home = home.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let mut buf = vec![0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/");
                    let method = request.split_whitespace().next().unwrap_or("GET");
                    if let Some(response) = ui_asset_response(&request, path, port) {
                        let _ = socket.write_all(&response).await;
                        return;
                    }

                    let (status, ctype, body) = if !request_is_local(&request, port) {
                        ("403 Forbidden", "text/plain", "refused".to_owned())
                    } else if method == "POST" && path == "/action/activate" {
                        let key = request.split("\r\n\r\n").nth(1).unwrap_or("").trim();
                        match licence::activate(&home, key) {
                            Ok(l) => (
                                "200 OK",
                                "application/json",
                                format!(r#"{{"ok":true,"name":"{}"}}"#, l.name.replace('"', "'")),
                            ),
                            Err(e) => (
                                "200 OK",
                                "application/json",
                                format!(
                                    r#"{{"ok":false,"error":"{}"}}"#,
                                    e.to_string().replace('"', "'")
                                ),
                            ),
                        }
                    } else if path == "/state" {
                        // Enough for the page to render itself and show the result.
                        (
                            "200 OK",
                            "application/json",
                            format!(
                                r#"{{"version":"{}","name":"this machine","id":"","platform":"{}/{}","role":"not activated","port":{port},"seamPort":0,"licence":{},"update":null,"activity":[],"peers":[],"health":[]}}"#,
                                env!("CARGO_PKG_VERSION"),
                                std::env::consts::OS,
                                std::env::consts::ARCH,
                                licence_json(&home),
                            ),
                        )
                    } else if let Some((_, page, ctype)) =
                        UI_PAGES.iter().find(|(route, _, _)| *route == path)
                    {
                        ("200 OK", *ctype, (*page).to_owned())
                    } else {
                        ("404 Not Found", "text/plain", "not found".to_owned())
                    };
                    // Images are bytes and must not go through a String. Served with a
                    // long cache lifetime because the logo changes with the binary, and
                    // the binary is what gets replaced on an update.
                    if let Some((_, bytes, ctype)) =
                        UI_ASSETS.iter().find(|(route, _, _)| *route == path)
                    {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                             Cache-Control: max-age=86400\r\nConnection: close\r\n\r\n",
                            bytes.len(),
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(bytes).await;
                        return;
                    }

                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
            () = tokio::time::sleep(Duration::from_secs(1)) => {
                if licence::stored(dir).is_some() {
                    println!("  activated — starting seam");
                    remove_ui_port_note_if_ours(dir);
                    return Ok(());
                }
            }
        }
    }
}

/// Wait for the elevated copy to actually be running, rather than assuming it is.
///
/// It proves itself by writing its own `ui-port` note, which happens once its page is up —
/// so this waits for that file to change from whatever was there before. Ten seconds is
/// generous for a local process and short enough that a refused UAC prompt does not leave
/// someone staring at a terminal.
#[cfg(target_os = "windows")]
fn elevated_child_started(dir: &std::path::Path) -> bool {
    let note = dir.join("ui-port");
    let before = std::fs::read_to_string(&note).unwrap_or_default();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(250));
        let now = std::fs::read_to_string(&note).unwrap_or_default();
        if !now.is_empty() && now != before {
            tracing::info!("the elevated copy is running; this one is done");
            return true;
        }
    }
    false
}

/// Ask the seam already running here to stop, so a newly launched one can take over.
///
/// Uses its own quit endpoint rather than signals: it is the same path the Quit button
/// takes, so input is released, the cursor is restored and its pages show the stopped
/// screen instead of hanging on a dead port.
fn stop_running_instance(dir: &std::path::Path) -> bool {
    use std::io::Write as _;
    let Ok(text) = std::fs::read_to_string(dir.join("ui-port")) else { return false };
    let Ok(port) = text.trim().parse::<u16>() else { return false };
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut socket) =
        std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500))
    else {
        return false;
    };
    let request = format!(
        "POST /action/quit HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).is_ok()
}

/// Is this bind failure "something is already listening"?
///
/// Matched on the message because the transport's error type deliberately does not leak
/// `std::io::ErrorKind`. Narrow on purpose: any other bind failure is still a real error
/// and must not be silently turned into "already running".
fn is_address_in_use(error: &seam_transport::Error) -> bool {
    let text = error.to_string();
    text.contains("Address already in use") || text.contains("os error 48")
        || text.contains("os error 98") || text.contains("10048")
}

/// Open the fleet page of the daemon running on this machine.
fn open_ui(dir: &std::path::Path) -> Result<()> {
    let port_file = dir.join("ui-port");
    let port: u16 = std::fs::read_to_string(&port_file)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .context("seam is not running on this machine — start it, then run 'seam ui'")?;
    // The file outlives the daemon that wrote it, so probe before opening a browser at
    // a dead port — 'connection refused' in a browser tab explains nothing.
    if std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(600),
    )
    .is_err()
    {
        let _ = std::fs::remove_file(&port_file);
        anyhow::bail!(
            "seam is not running on this machine (a previous run left its note behind) — \
             start 'seam run', then 'seam ui'"
        );
    }
    let url = format!("http://127.0.0.1:{port}/");
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(&url).spawn()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn()?;
    println!("{url}");
    Ok(())
}

/// The moment input was last seen. Used by the watchdog.
#[cfg(target_os = "macos")]
static LAST_INPUT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Give this machine its input back if events stop arriving while it is withheld.
///
/// This is the safety net for the class of failure that has locked this machine twice: if
/// anything stops the capture delivering events — macOS disabling a slow tap, a stall, a
/// bug — then focus can never return, and suppression stays on with the cursor detached.
/// From the user's chair the machine is simply dead, with no way back short of killing the
/// process.
///
/// So it is made time-bounded rather than depending on the event path that just failed.
/// Two seconds of silence while input is withheld is not a plausible state.
#[cfg(target_os = "macos")]
fn start_input_watchdog() {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(500));
        loop {
            ticker.tick().await;
            if !seam_input::macos::is_suppressing_local() {
                continue;
            }
            // Ask whether CAPTURE is alive, not whether the user has been busy.
            //
            // This used to release input after two seconds without events, on the reasoning
            // that silence while withheld was implausible. It is entirely plausible: it is
            // a person reading a page on the other machine without touching the mouse. The
            // watchdog then handed input back here, which reattached the cursor and cleared
            // suppression - so the cursor tracked the mouse again on this machine and
            // keystrokes landed here as well as on the remote one. It fired twice in a
            // single test session and was the cause of both complaints.
            //
            // A disabled tap is unambiguous and does not depend on user activity.
            if seam_input::macos::capture_is_alive() == Some(false) {
                tracing::error!(
                    "the input tap is disabled while this machine's input was withheld — \
                     releasing it so the machine stays usable"
                );
                seam_input::release_input();
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn start_input_watchdog() {}

// `park_cursor` used to live here, moving the local cursor to a corner so it was not
// visible while another machine had the pointer. It is deliberately gone.
//
// Warping the cursor perturbs the delta reported by the following event, which crossed a
// screen edge, which parked again — a feedback loop running at event rate. The log showed
// focus alternating between two machines every 20 ms, which from the user's chair is
// indistinguishable from the mouse and keyboard being frozen. It locked the machine twice.
//
// The cursor sitting at the edge while another machine has the pointer is a cosmetic
// problem. Anything that writes cursor position from inside the loop that *reads* cursor
// movement is a correctness problem, and this one was not worth its cost. Hiding it
// properly needs foreground status, which a daemon does not have; the real fix is a UI
// agent, not another warp.

/// Shared clipboard state.
type Clipboard = Arc<tokio::sync::Mutex<ClipboardState>>;

#[derive(Default, Debug)]
struct ClipboardState {
    /// The last text seen on this machine, whether typed here or received from a peer.
    last_seen: Option<String>,
    /// Highest generation applied, PER PEER.
    ///
    /// Generations are each machine's own counter, so comparing them across machines is
    /// meaningless: a peer that restarts begins at 1, and a machine that has been running
    /// for hours is at 40 — every frame from the fresh peer then looks older than what is
    /// already applied and is silently dropped. Keyed by peer, the counter does what it
    /// was for: recognising that machine's own echo.
    applied: Vec<(seam_proto::PeerId, u64)>,
    /// This machine's own change counter.
    generation: u64,
    /// Signature of the last image seen or applied, so an echo is recognised without
    /// keeping megabytes of pixels around.
    image_sig: Option<u64>,
    /// Signature of the last file list seen or applied, same purpose.
    files_sig: Option<u64>,
}

/// Total cap for clipboard file transfers. A copy over this is refused with a log
/// line, never truncated — half a folder is worse than none.
const MAX_CLIPBOARD_FILE_BYTES: usize = 64 * 1024 * 1024;

/// Signature of what the file clipboard points at: paths, sizes and mtimes — no
/// contents — so the poll stays cheap no matter how big the copy is.
fn files_signature(paths: &[std::path::PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    for path in paths {
        path.hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(path) {
            meta.len().hash(&mut hasher);
            if let Ok(modified) = meta.modified() {
                modified.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Read the copied files into wire entries: path-relative-to-the-copy plus bytes.
/// Folders are walked; symlinks are skipped — a clipboard copy is contents, not
/// filesystem structure.
fn gather_clipboard_files(
    paths: &[std::path::PathBuf],
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut entries = Vec::new();
    let mut total: usize = 0;
    let mut add = |rel: String, bytes: Vec<u8>, total: &mut usize| {
        *total = total.saturating_add(bytes.len());
        entries.push((rel, bytes));
    };
    for top in paths {
        let Some(name) = top.file_name() else { continue };
        let name = name.to_string_lossy().into_owned();
        let meta = std::fs::symlink_metadata(top)
            .map_err(|e| format!("{} is unreadable: {e}", top.display()))?;
        if meta.is_file() {
            let bytes =
                std::fs::read(top).map_err(|e| format!("{} is unreadable: {e}", top.display()))?;
            add(name, bytes, &mut total);
        } else if meta.is_dir() {
            let mut stack = vec![(top.clone(), name)];
            while let Some((dir, rel)) = stack.pop() {
                let listing = std::fs::read_dir(&dir)
                    .map_err(|e| format!("{} is unreadable: {e}", dir.display()))?;
                for entry in listing {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let path = entry.path();
                    let child = format!("{rel}/{}", entry.file_name().to_string_lossy());
                    let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                    if meta.is_file() {
                        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                        add(child, bytes, &mut total);
                    } else if meta.is_dir() {
                        stack.push((path, child));
                    }
                }
            }
        }
        if total > MAX_CLIPBOARD_FILE_BYTES {
            return Err(format!(
                "the copy is over {} MB; a clipboard is for documents, not disks",
                MAX_CLIPBOARD_FILE_BYTES / (1024 * 1024)
            ));
        }
    }
    Ok(entries)
}

/// Write received entries under the state directory and return the top-level paths to
/// put on this machine's clipboard. The previous receipt is swept first, so pasted
/// copies do not accumulate forever.
fn spool_clipboard_files(
    generation: u64,
    entries: &[(String, Vec<u8>)],
) -> Result<Vec<std::path::PathBuf>> {
    let root = store::state_dir()?.join("clipboard-files");
    if let Ok(previous) = std::fs::read_dir(&root) {
        for old in previous.flatten() {
            let _ = std::fs::remove_dir_all(old.path());
        }
    }
    let dir = root.join(format!("gen-{generation}"));
    std::fs::create_dir_all(&dir)?;
    let mut tops: Vec<std::path::PathBuf> = Vec::new();
    for (rel, bytes) in entries {
        let relpath = std::path::Path::new(rel);
        // A received path is untrusted input. Anything but plain components — absolute
        // paths, drive prefixes, '..' — could escape the spool and overwrite real files.
        let mut components = relpath.components();
        let Some(first @ std::path::Component::Normal(_)) = components.next() else {
            anyhow::bail!("refusing a path that could escape the spool: {rel:?}");
        };
        if components.clone().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            anyhow::bail!("refusing a path that could escape the spool: {rel:?}");
        }
        let dest = dir.join(relpath);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, bytes)?;
        let top = dir.join(first.as_os_str());
        if !tops.contains(&top) {
            tops.push(top);
        }
    }
    Ok(tops)
}

impl ClipboardState {
    /// Has this peer already sent us this generation, or a newer one?
    fn already_applied(&self, peer: seam_proto::PeerId, generation: u64) -> bool {
        self.applied.iter().any(|(p, g)| *p == peer && *g >= generation)
    }

    /// Record what was applied from a peer.
    fn record(&mut self, peer: seam_proto::PeerId, generation: u64) {
        self.applied.retain(|(p, _)| *p != peer);
        self.applied.push((peer, generation));
    }

    /// A peer's link was just (re)registered — its counter may have been reborn.
    ///
    /// Generations are a process's own counter, so a watermark outlives the process it
    /// described: a laptop that restarts begins again at 1, and every copy it makes is
    /// then "older" than the 40 remembered from its previous life and silently dropped.
    /// Observed live: after a laptop restart, its copies reached this machine and
    /// vanished at the gate — no receipt logged, nothing applied, nothing relayed.
    fn forget_peer(&mut self, peer: seam_proto::PeerId) {
        self.applied.retain(|(p, _)| *p != peer);
    }
}

/// Compress pixels for the wire. Protocol v2: `ClipboardImage.rgba` carries deflate.
///
/// Raw RGBA was killing the fleet. A copied screenshot is 14–17 MB of uncompressed
/// pixels, fanned out to every peer at once; on Wi-Fi that saturates the link long
/// enough that keep-alive acks stop arriving, the 15-second patience runs out, and the
/// transfer strangles the very connection carrying it — measured live twice, on two
/// evenings: "clipboard changed; sharing kind=files/image", then every peer timing out
/// within the minute, the pointer trapped on whichever machine held it. Screenshots
/// deflate 5–10×, which turns the blast into something a radio can carry politely.
fn deflate_pixels(rgba: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    // Writing into a Vec cannot fail; the unwrap_or covers the trait's signature.
    let _ = encoder.write_all(rgba);
    encoder.finish().unwrap_or_default()
}

/// Decompress wire pixels, refusing anything that does not inflate to exactly
/// `width * height * 4` bytes — a mismatch is corruption, not something to guess at.
fn inflate_pixels(wire: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    // A screen larger than this does not exist; a claim that one does is an attack or
    // corruption, and either way not worth the allocation.
    if expected > 512 * 1024 * 1024 {
        return None;
    }
    let mut raw = Vec::with_capacity(expected.min(64 * 1024 * 1024));
    let decoder = flate2::read::ZlibDecoder::new(wire);
    // The take() bound keeps a corrupt stream from inflating without limit.
    let mut bounded = decoder.take(expected as u64 + 1);
    bounded.read_to_end(&mut raw).ok()?;
    (raw.len() == expected).then_some(raw)
}

/// Compress one file's bytes for the wire: an 8-byte length, then deflate.
///
/// Files got the same disease as images one release later: "sharing kind=files", then
/// the peer timing out twenty-one seconds after — the payload saturating the link that
/// carried it. The length prefix is what lets the receiver refuse a stream that does
/// not inflate to exactly what was promised.
fn deflate_file_bytes(raw: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut wire = (raw.len() as u64).to_le_bytes().to_vec();
    let mut encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = encoder.write_all(raw);
    wire.extend(encoder.finish().unwrap_or_default());
    wire
}

/// Inverse of [`deflate_file_bytes`]; `None` on any mismatch or absurd claim.
fn inflate_file_bytes(wire: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let (len_bytes, deflated) = wire.split_at_checked(8)?;
    let expected = usize::try_from(u64::from_le_bytes(len_bytes.try_into().ok()?)).ok()?;
    if expected > 256 * 1024 * 1024 {
        return None;
    }
    let mut raw = Vec::with_capacity(expected.min(64 * 1024 * 1024));
    let mut bounded = flate2::read::ZlibDecoder::new(deflated).take(expected as u64 + 1);
    bounded.read_to_end(&mut raw).ok()?;
    (raw.len() == expected).then_some(raw)
}

/// Cheap identity for clipboard images: dimensions plus a streaming hash of the pixels.
/// Collisions would only cost a skipped share of a near-identical image.
fn image_signature(width: u32, height: u32, rgba: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (width, height, rgba).hash(&mut hasher);
    hasher.finish()
}

/// Send one frame to every connected peer, logging once.
async fn share_with_all(
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    frame: &seam_proto::Frame,
    what: &str,
) {
    let peers = links.lock().await;
    if peers.is_empty() {
        return;
    }
    tracing::info!(peers = peers.len(), kind = what, "clipboard changed; sharing");
    for link in peers.iter() {
        if let Err(e) = link.send_reliable(frame).await {
            tracing::warn!(peer = %link.peer_id(), kind = what, "could not share the clipboard: {e}");
        }
    }
}

/// Watch this machine's clipboard and share every change with all peers.
///
/// **All peers, not just the focused one.** A clipboard is not a pointer: copying on one
/// machine and pasting on another is the whole point, and requiring the pointer to be
/// somewhere first would defeat it.
///
/// macOS offers no clipboard-change notification — `NSPasteboard.changeCount` polling is
/// the only supported mechanism — so this polls. 300 ms is below the threshold where a
/// person notices, and the read is cheap.
fn start_clipboard_sync(links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>, clipboard: Clipboard) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(300));
        // Seed with whatever is already on the clipboard, so starting the daemon does not
        // immediately broadcast the user's existing clipboard to every machine.
        if let Ok(text) = seam_input::clipboard::read_text() {
            clipboard.lock().await.last_seen = text;
        }
        if let Ok(Some((w, h, rgba))) = seam_input::clipboard::read_image() {
            clipboard.lock().await.image_sig = Some(image_signature(w, h, &rgba));
        }
        if let Ok(Some(paths)) = seam_input::clipboard::read_file_list() {
            clipboard.lock().await.files_sig = Some(files_signature(&paths));
        }

        loop {
            ticker.tick().await;

            // Files before text, deliberately: Finder puts a copied file's NAME on the
            // pasteboard as text as well, so a text-first check would share the
            // filename and mask the files themselves.
            let (share_text, share_images, share_files) =
                UI_SHARES.lock().map_or((true, true, true), |shares| *shares);

            if share_files
                && let Ok(Some(paths)) = seam_input::clipboard::read_file_list()
            {
                let sig = files_signature(&paths);
                if clipboard.lock().await.files_sig == Some(sig) {
                    continue;
                }
                match gather_clipboard_files(&paths) {
                    Ok(entries) if !entries.is_empty() => {
                        let frame = {
                            let mut state = clipboard.lock().await;
                            state.files_sig = Some(sig);
                            state.generation += 1;
                            seam_proto::Frame::ClipboardFiles {
                                seq: 0,
                                generation: state.generation,
                                entries: entries
                                    .into_iter()
                                    .map(|(name, raw)| (name, deflate_file_bytes(&raw)))
                                    .collect(),
                            }
                        };
                        note_transfer("sending files", format!("{} files", paths.len()));
                        share_with_all(&links, &frame, "files").await;
                        note_transfer("files sent", format!("{} files", paths.len()));
                    }
                    Ok(_) => clipboard.lock().await.files_sig = Some(sig),
                    Err(reason) => {
                        // Remember the signature regardless, or this repeats every poll.
                        clipboard.lock().await.files_sig = Some(sig);
                        tracing::warn!(
                            files = paths.len(),
                            "not sharing the copied files: {reason}"
                        );
                    }
                }
                continue;
            }

            // Text next: cheapest to read, and by far the most common.
            if share_text
                && let Ok(Some(text)) = seam_input::clipboard::read_text()
            {
                let frame = {
                    let mut state = clipboard.lock().await;
                    if state.last_seen.as_deref() == Some(text.as_str()) {
                        continue;
                    }
                    state.last_seen = Some(text.clone());
                    state.generation += 1;
                    seam_proto::Frame::ClipboardText { seq: 0, generation: state.generation, text }
                };
                share_with_all(&links, &frame, "text").await;
                continue;
            }

            // No text — perhaps an image; a screenshot replaces text on the clipboard.
            // Reading an image copies the pixels out, which is why it is only tried
            // when text is absent, and why identity is a hash rather than the bytes.
            if !share_images {
                continue;
            }
            let Ok(Some((width, height, rgba))) = seam_input::clipboard::read_image() else {
                continue;
            };
            let sig = image_signature(width, height, &rgba);
            let frame = {
                let mut state = clipboard.lock().await;
                if state.image_sig == Some(sig) {
                    continue;
                }
                state.image_sig = Some(sig);
                state.generation += 1;
                seam_proto::Frame::ClipboardImage {
                    seq: 0,
                    generation: state.generation,
                    width,
                    height,
                    rgba: deflate_pixels(&rgba),
                }
            };
            share_with_all(&links, &frame, "image").await;
        }
    });
}

/// Dial every already-paired machine discovery finds.
///
/// Split out of `daemon` for length. Advertising without dialling meant two daemons could
/// see each other and sit there — which is exactly what "no machines connected yet" on
/// both screens looked like. Only already-trusted peers are dialled, and `register_link`
/// drops any duplicate, so a peer that is also dialling us costs one redundant
/// connection, not a loop.
fn start_auto_dial(
    discovery: &Discovery,
    store: &SharedTrust,
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    geometry: &Geometry,
    clipboard: &Clipboard,
    endpoint: &Arc<seam_transport::Endpoint>,
) {
    match discovery.browse() {
        Ok(found) => {
            let store = Arc::clone(store);
            let links = Arc::clone(links);
            let geometry = Arc::clone(geometry);
            let clipboard = Arc::clone(clipboard);
            let endpoint = Arc::clone(endpoint);
            tokio::spawn(async move {
                let own = endpoint.identity().peer_id();
                // A paired machine can drop off the links with no discovery event: its
                // record is still cached, so Found never re-fires — and if ITS dials
                // are blackholed (a dual-homed peer answering from the wrong interface,
                // a stale ARP entry) nothing on this side ever tried again. Observed
                // live: a laptop placed and entered at 18:02:59, dead at 18:03:03, and
                // stranded for minutes on an address this machine never redialled. The
                // sweep retries every trusted, visible, absent machine on a slow tick.
                let mut resweep = tokio::time::interval(Duration::from_secs(20));
                resweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    let event = tokio::select! {
                        event = found.next() => match event {
                            Some(event) => event,
                            None => break,
                        },
                        _ = resweep.tick() => {
                            redial_absent_peers(&endpoint, &store, &links, &geometry, &clipboard)
                                .await;
                            continue;
                        }
                    };
                    let peer = match event {
                        seam_transport::DiscoveryEvent::Found(peer) => peer,
                        seam_transport::DiscoveryEvent::Lost { instance } => {
                            if let Ok(mut seen) = UI_DISCOVERED.lock() {
                                seen.retain(|m| m.instance != instance);
                            }
                            continue;
                        }
                    };
                    if peer.advertised_peer_id == Some(own) {
                        continue;
                    }
                    let trusted = peer.advertised_fingerprint.is_some_and(|fingerprint| {
                        store.read().is_ok_and(|s| s.is_trusted(fingerprint))
                    });
                    // The pairing page lists everyone on the network, stranger or not.
                    if let (Ok(mut seen), Some(&addr)) =
                        (UI_DISCOVERED.lock(), peer.addresses.first())
                    {
                        seen.retain(|m| m.instance != peer.instance);
                        seen.push(DiscoveredMachine {
                            instance: peer.instance.clone(),
                            short: peer
                                .advertised_peer_id
                                .map_or_else(|| peer.name.clone(), |id| id.to_string()),
                            addr,
                            trusted,
                        });
                    }
                    // Trust is still decided by the handshake; this only decides who is
                    // worth dialling, so an unpaired machine is skipped, not refused —
                    // with one exception. A seam that trusts NOBODY yet is a machine
                    // someone just installed: it offers itself to whatever fleet it can
                    // see, which puts the six digits on both screens with nothing typed
                    // anywhere. A machine with even one pairing never volunteers.
                    if !trusted {
                        let lonely = store.read().is_ok_and(|s| s.is_empty());
                        let idle = UI_PAIRING.lock().is_ok_and(|slot| slot.is_none());
                        if lonely && idle {
                            for address in &peer.addresses {
                                if let Ok(link) = endpoint.connect(*address).await {
                                    park_pairing(link);
                                    break;
                                }
                            }
                        }
                        continue;
                    }
                    let already = links.lock().await.iter().any(|link| {
                        peer.advertised_peer_id.is_some_and(|id| id == link.peer_id())
                    });
                    if already {
                        continue;
                    }
                    tracing::info!(peer = %peer.name, "found a paired machine; connecting");
                    dial_trusted(
                        &endpoint,
                        &store,
                        &links,
                        &geometry,
                        &clipboard,
                        &peer.name,
                        &peer.addresses,
                    )
                    .await;
                }
            });
        }
        Err(e) => tracing::warn!("discovery unavailable; only --connect addresses work: {e}"),
    }
}

/// One sweep pass: redial every trusted machine that is visible but not connected.
async fn redial_absent_peers(
    endpoint: &Arc<seam_transport::Endpoint>,
    store: &SharedTrust,
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    geometry: &Geometry,
    clipboard: &Clipboard,
) {
    let connected: Vec<String> =
        links.lock().await.iter().map(|l| l.peer_id().to_string()).collect();
    let absent: Vec<(String, std::net::SocketAddr)> = UI_DISCOVERED
        .lock()
        .map(|seen| {
            seen.iter()
                .filter(|m| m.trusted && !connected.iter().any(|c| c.starts_with(&m.short)))
                .map(|m| (m.short.clone(), m.addr))
                .collect()
        })
        .unwrap_or_default();
    for (short, addr) in absent {
        tracing::info!(peer = %short, %addr, "a paired machine is visible but not connected; dialling it");
        dial_trusted(endpoint, store, links, geometry, clipboard, &short, &[addr]).await;
    }
}

/// Dial a trusted machine at the given addresses; register the first that answers as
/// itself. Every failed address is named — that exact silence once cost an afternoon.
async fn dial_trusted(
    endpoint: &Arc<seam_transport::Endpoint>,
    store: &SharedTrust,
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    geometry: &Geometry,
    clipboard: &Clipboard,
    label: &str,
    addresses: &[std::net::SocketAddr],
) {
    for address in addresses {
        let link = match endpoint.connect(*address).await {
            Ok(link) => link,
            Err(e) => {
                tracing::info!(peer = %label, %address, "no luck at this address: {e}");
                continue;
            }
        };
        // `presented` is the whole diagnosis when an address is answered by the wrong
        // machine: a stale entry after standby points at a box that completes the
        // handshake happily — as someone else.
        let verdict = store
            .read()
            .map_or(Err(seam_transport::Error::NotPaired), |s| link.authorize(&s));
        if let Err(e) = verdict {
            tracing::warn!(peer = %label, %address, presented = %link.peer_id(), "answered by a machine we are not paired with: {e}");
            link.close("not paired");
            continue;
        }
        tracing::info!(peer = %link.peer_id(), %address, "found and connected");
        let link = Arc::new(link);
        register_link(links, &link, clipboard).await;
        announce_geometry(&link).await;
        tokio::spawn(receive_from(
            link,
            Arc::clone(geometry),
            Arc::clone(clipboard),
            Arc::clone(links),
        ));
        return;
    }
}

/// Peer id to real screen size, as reported by that peer.
type Geometry = Arc<tokio::sync::Mutex<std::collections::HashMap<seam_proto::PeerId, (i32, i32)>>>;

/// Tell a peer how big this machine's screen is.
///
/// Screen size is detected, never configured (goal Z2) — but it must also be *exchanged*.
/// Without it the machine that owns the pointer places the screen boundary at an invented
/// coordinate, and a boundary in the wrong place is indistinguishable from a pointer that
/// refuses to leave.
async fn announce_geometry(link: &Link) {
    let Ok(desktop) = seam_input::desktop() else { return };
    let bb = desktop.bounding_box();
    let hello = seam_proto::Frame::Hello(seam_proto::Hello {
        version: seam_proto::PROTOCOL_VERSION,
        peer: seam_proto::PeerId::NIL,
        // The display name carries this machine's seam version after a tab. The field is
        // already free-form display text, so this needs no new frame and no protocol
        // change — and a fleet that must run matching versions has no way to show which
        // machine is behind without it. Older peers send a bare name and are reported as
        // an unknown version rather than assumed to match.
        name: format!("\t{}", env!("CARGO_PKG_VERSION")),
        width: u32::try_from(bb.width).unwrap_or(0),
        height: u32::try_from(bb.height).unwrap_or(0),
        scale: desktop.primary().map_or(256, |d| d.scale),
        layout_policy: seam_proto::LayoutPolicy::Auto,
    });
    if let Err(e) = link.send_reliable(&hello).await {
        tracing::warn!(peer = %link.peer_id(), "could not report this machine's screen: {e}");
    }
}

/// Add a peer's link, replacing any earlier link to the same peer.
///
/// A machine that reconnects - because its daemon was restarted, or the network blipped -
/// arrives as a second link with the same `PeerId`. Pushing it left the dead one in the
/// list ahead of it, and everything that iterates peers then talked to the corpse: the
/// Mac mini's log showed `clipboard changed; sharing peers=3` with only two machines on
/// the desk, and the reconnected laptop received nothing while no send ever errored.
///
/// Replacing by identity is correct rather than merely tidy: `PeerId` is derived from the
/// peer's certificate, so two links with the same id are the same machine by construction.
async fn register_link(
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    link: &Arc<Link>,
    clipboard: &Clipboard,
) {
    let id = link.peer_id();
    let replaced: Vec<Arc<Link>> = {
        let mut peers = links.lock().await;
        let replaced = peers.iter().filter(|l| l.peer_id() == id).map(Arc::clone).collect();
        peers.retain(|l| l.peer_id() != id);
        peers.push(Arc::clone(link));
        replaced
    };
    for stale in &replaced {
        // Close it, don't just forget it. A replaced link left open is a zombie: its
        // tasks keep reading, the other side may keep SENDING on it, and the two
        // machines can end up talking through different connections — one of which
        // nobody is reading. One peer, one link, and the loser is told it lost.
        stale.close("replaced by a newer link");
    }
    if !replaced.is_empty() {
        tracing::info!(peer = %id, "peer reconnected; closed the stale link");
    }
    // A fresh link may be a fresh process, and a fresh process counts from one again.
    clipboard.lock().await.forget_peer(id);

    // A machine arriving is as much a change to the desk as someone moving one.
    //
    // Without this the layout was only ever rebuilt when a position was edited, so a
    // laptop that slept came back holding the placement it had before — stale geometry,
    // and often a placement made while its neighbour was away. Sharing stayed broken until
    // the arrangement was changed to something wrong and back again, which was the only
    // thing that forced a rebuild. That is a precise description of this bug, and it was
    // how the user found it.
    UI_RELAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
}


/// Keep one outbound connection alive, retrying until it exists and after it drops.
///
/// Split out of `daemon` for length. Connecting once at startup made the order the
/// machines were switched on significant: a client launched before its server logged
/// "could not connect" and then did nothing for the whole session.
#[allow(clippy::too_many_arguments)]
fn spawn_reconnector(
    target: SocketAddr,
    port: u16,
    endpoint: Arc<seam_transport::Endpoint>,
    store: SharedTrust,
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    geometry: Geometry,
    clipboard: Clipboard,
) {
    tokio::spawn(async move {
            // Back off up to 5 s: fast enough that starting the server feels immediate,
            // slow enough not to spin when nothing is there.
            let mut delay = Duration::from_millis(500);
            let mut failures = 0u32;
            loop {
                match endpoint.connect(target).await {
                    // Keep the lane alive on a refusal. Returning turned one refusal
                    // into a permanent disconnection: a peer answering mid-restart
                    // before its trust store loads, or an address answered by the wrong
                    // machine after standby, killed this loop for good and nothing ever
                    // dialled again. `presented` names who actually answered, which is
                    // the whole diagnosis when it is not who the address used to be.
                    Ok(link)
                        if store.read().is_ok_and(|s| link.authorize(&s).is_err()) =>
                    {
                        let refusal = store
                            .read()
                            .map_or(Err(seam_transport::Error::NotPaired), |s| {
                                link.authorize(&s)
                            })
                            .expect_err("checked in the guard");
                        let lonely = store.read().is_ok_and(|s| s.is_empty());
                        if lonely && matches!(refusal, seam_transport::Error::NotPaired) {
                            // A machine told to connect somewhere before it trusts
                            // anyone is a machine someone is joining to a fleet: put
                            // the six digits on both screens rather than refusing the
                            // very thing the person asked for.
                            park_pairing(link);
                        } else {
                            if failures == 0 {
                                tracing::warn!(%target, presented = %link.peer_id(), "refused: {refusal}; keeping this lane alive");
                            } else {
                                tracing::debug!(%target, presented = %link.peer_id(), "still refused: {refusal}");
                            }
                            link.close("not paired");
                        }
                    }
                    Ok(link) => {
                        tracing::info!(peer = %link.peer_id(), %target, "connected to peer");
                        if let Ok(mut dialled) = UI_DIALLED.lock() {
                            let id = link.peer_id();
                            if !dialled.contains(&id) {
                                dialled.push(id);
                            }
                        }
                        delay = Duration::from_millis(500);
                        failures = 0;
                        let link = Arc::new(link);
                        register_link(&links, &link, &clipboard).await;
                        announce_geometry(&link).await;
                        // Returns when the peer goes away, and then we try again.
                        receive_from(
                            Arc::clone(&link),
                            Arc::clone(&geometry),
                            Arc::clone(&clipboard),
                            Arc::clone(&links),
                        )
                        .await;
                        tracing::info!(%target, "peer went away; reconnecting");
                    }
                    Err(e) => {
                        // The first failure of a streak is worth a visible line — it is
                        // the moment something changed. The rest stay quiet: this loop
                        // runs every few seconds against a machine that may simply be
                        // switched off.
                        if failures == 0 {
                            tracing::warn!(%target, "could not connect: {e}; retrying quietly");
                        } else {
                            tracing::debug!(%target, "not reachable yet ({e}); retrying");
                        }
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));

                // A run of failures usually means this machine slept: the socket bound
                // before standby survives, accepts writes and reaches nothing, so every
                // attempt fails while looking like an unreachable peer. Rebinding gets a
                // socket on the network that exists now, keeping this machine's identity
                // so peers still recognise it.
                failures += 1;
                if failures.is_multiple_of(5) {
                    match endpoint.rebind(port) {
                        Ok(()) => tracing::info!(
                            "rebound the network socket after repeated failures (a sleep \
                             or a network change looks exactly like this)"
                        ),
                        Err(e) => tracing::debug!("could not rebind: {e}"),
                    }
                }
            }
    });
}

/// Is a machine sitting somewhere it does not belong?
///
/// A machine whose neighbour was away is attached to this one so it stays reachable. Once
/// that neighbour is back, the stored relation must be restored — otherwise the fallback
/// quietly becomes the arrangement, which is a layout that changes itself after a laptop
/// sleeps.
fn layout_needs_rebuild(
    dir: &std::path::Path,
    live: &[seam_proto::PeerId],
    known: &[seam_proto::PeerId],
) -> bool {
    let here = |id: &str| live.iter().any(|p| p.to_string().starts_with(id));
    let placed = |id: &str| known.iter().any(|p| p.to_string().starts_with(id));
    stored_layout(dir)
        .into_iter()
        .any(|(peer, _, anchor)| {
            anchor != "self" && here(&peer) && here(&anchor) && placed(&peer) && !placed(&anchor)
        })
}

/// Keep the layout in step with which peers are actually connected.
///
/// Peers are placed to the right in the order they connect. A layout editor belongs in
/// the UI; this makes handover work today without asking the user to draw anything
/// (goal Z3). A peer that goes away is removed, which returns the pointer home if it
/// held it — never leave input aimed at a machine that is gone (goal R2).
#[cfg(target_os = "macos")]
async fn sync_peers(
    links: &Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    graph: &mut focus::Graph,
    known: &mut Vec<seam_proto::PeerId>,
    geometry: &Geometry,
    dir: &std::path::Path,
) {
    // An arrangement change means every placement is stale, so start again rather than
    // patching one screen and leaving the rest describing the old desk.
    let live: Vec<seam_proto::PeerId> = links.lock().await.iter().map(|l| l.peer_id()).collect();

    let waiting_for = layout_needs_rebuild(dir, &live, known);

    if waiting_for || UI_RELAYOUT.swap(false, std::sync::atomic::Ordering::Relaxed) {
        // Clearing `known` alone was not enough: the graph kept its old node, so
        // `is_placed` still answered yes and the peer was skipped rather than re-placed
        // with the geometry and neighbour it has now.
        known.clear();
        graph.forget_all();
        if let Ok(mut places) = UI_PLACES.lock() {
            places.clear();
        }
    }

    let sizes = geometry.lock().await.clone();

    // Place in PAIRING order, never connection order. Connection order is a race, and
    // losing it meant the laptop took the iMac's Left edge and the iMac was left with
    // nowhere to sit. Pairing order is a deliberate human act, recorded once.
    let order = store::pairing_order(dir);
    let mut ordered: Vec<seam_proto::PeerId> =
        order.iter().copied().filter(|id| live.contains(id)).collect();
    // Anything paired before this build wrote an ordering still gets placed, after those
    // that have one, so an upgrade never strands a peer.
    ordered.extend(live.iter().copied().filter(|id| !order.contains(id)));

    for id in &ordered {
        if graph.is_placed(*id) {
            continue;
        }
        // Wait for the peer's real screen size rather than guessing one.
        let Some(&(w, h)) = sizes.get(id) else { continue };
        // Default arrangement, matching this project's own fleet and the commonest desk
        // layout: the first machine sits to the LEFT, the next one BELOW that. A layout
        // editor belongs in the UI; this makes handover work without asking anyone to
        // draw anything (goal Z3).
        // A person's answer outranks the guess — and it is a relation, so the machine it
        // is placed against has to be on the desk first. If it is not yet, this one waits
        // for a later pass rather than being anchored to something arbitrary.
        if let Some((_, edge, anchor)) = stored_layout(dir)
            .into_iter()
            .find(|(peer, _, _)| id.to_string().starts_with(peer.as_str()))
        {
            let chosen = match edge.as_str() {
                "left" => focus::Edge::Left,
                "right" => focus::Edge::Right,
                "top" => focus::Edge::Top,
                _ => focus::Edge::Bottom,
            };
            let against = if anchor == "self" {
                None
            } else if let Some(peer) =
                known.iter().find(|k| k.to_string().starts_with(anchor.as_str()))
            {
                Some(*peer)
            } else if live.iter().any(|k| k.to_string().starts_with(anchor.as_str())) {
                // Its neighbour is connected but not placed yet — it is earlier in this
                // same pass. Come back to this one; the order settles within a tick.
                continue;
            } else {
                // Its neighbour is not here at all: asleep, or switched off. Attaching to
                // this machine instead keeps the machine reachable, and the stored
                // arrangement is untouched, so the moment its neighbour returns the
                // relation is restored exactly as it was.
                //
                // Skipping it instead - which is what happened - left the machine with no
                // place on the desk, so the pointer could not reach it and the layout
                // looked like it had changed itself after a laptop slept.
                tracing::info!(
                    peer = %id,
                    %anchor,
                    "its neighbour is away; attaching to this machine until it returns"
                );
                None
            };
            graph.place(*id, chosen, against, w, h);
            if let Ok(mut places) = UI_PLACES.lock() {
                places.retain(|(p, _)| p != id);
                places.push((
                    *id,
                    match chosen {
                        focus::Edge::Left => "left",
                        focus::Edge::Right => "right",
                        focus::Edge::Top => "top",
                        focus::Edge::Bottom => "bottom",
                    },
                ));
            }
            known.push(*id);
            tracing::info!(peer = %id, %edge, %anchor, "placed where you said it sits");
            continue;
        }

        let (edge, anchor) = match known.first() {
            None => (focus::Edge::Left, None),
            Some(first) => (focus::Edge::Bottom, Some(*first)),
        };
        // A later-paired peer hangs below the first one, so it cannot be placed until the
        // first one is. Waiting is correct: taking the free Left edge instead is exactly
        // the bug this ordering exists to stop.
        if anchor.is_some_and(|a| !graph.is_placed(a)) {
            continue;
        }
        graph.place(*id, edge, anchor, w, h);
        if let Ok(mut places) = UI_PLACES.lock() {
            places.retain(|(p, _)| p != id);
            places.push((
                *id,
                match edge {
                    focus::Edge::Left => "left",
                    focus::Edge::Right => "right",
                    focus::Edge::Top => "top",
                    focus::Edge::Bottom => "bottom",
                },
            ));
        }
        known.push(*id);
        tracing::info!(peer = %id, ?edge, w, h, "placed — push the pointer off that edge to reach it");
    }

    known.retain(|id| {
        if live.contains(id) {
            return true;
        }
        tracing::info!(peer = %id, "peer gone; input returns to this machine");
        graph.forget(*id);
        false
    });
}

/// Apply a change of ownership: freeze or release this machine's input.
#[cfg(target_os = "macos")]
fn handover(
    update: focus::Update,
    detached: Option<seam_input::macos::CursorGuard>,
) -> Option<seam_input::macos::CursorGuard> {
    use focus::Focus;
    match update.focus {
        Focus::Remote(peer) => {
            tracing::info!(%peer, "pointer and keyboard moved to this peer");
            // Release any previous guard FIRST. Its `Drop` clears suppression, so holding
            // it while enabling suppression means moving from one peer to another
            // silently switches this machine's input back on — which looks exactly like
            // every machine responding at once.
            // Keep the SAME guard across a peer-to-peer hop.
            //
            // Dropping it reattaches the cursor to the mouse, and creating the next one
            // detaches it again. Between those two calls the cursor is live, so every hop
            // - Mac to iMac to laptop - opened a window where local movement showed on
            // this machine. Measurement says the detach itself works from a daemon
            // (`does_detaching_actually_freeze_the_cursor` reports FROZE), so these
            // windows are what is left.
            //
            // The cursor only needs to be reattached when input comes *home*, which is
            // the `Focus::Local` arm below.
            if let Some(guard) = detached {
                seam_input::macos::set_suppress_local(true);
                return Some(guard);
            }
            // Hiding is attempted, but `CGDisplayHideCursor` only affects the cursor for
            // a *foreground* application, and a daemon is not one — so it silently does
            // nothing here. The cursor is therefore parked out of the way instead, which
            // is what Barrier and Synergy do for the same reason.
            let guard = match seam_input::macos::CursorGuard::detach(true) {
                Ok(guard) => Some(guard),
                Err(e) => {
                    // Worth saying out loud: without the detach the local cursor keeps
                    // tracking the mouse, and the user sees it wander while another
                    // machine has the pointer.
                    tracing::warn!("could not freeze this machine's cursor: {e}");
                    None
                }
            };
            seam_input::macos::set_suppress_local(true);
            if let Ok(mut f) = UI_FOCUS.lock() {
                *f = Some(peer);
            }
            guard
        }
        Focus::Local => {
            tracing::info!("pointer and keyboard back on this machine");
            if let Ok(mut f) = UI_FOCUS.lock() {
                *f = None;
            }
            seam_input::macos::set_suppress_local(false);
            drop(detached);
            // Put the real cursor where the layout says the pointer is. It has been frozen
            // at the edge since focus left, and the next event adopts the OS position as
            // truth — so without this the stale position immediately drags focus back
            // across, and the pointer oscillates in a narrow band along the shared edge.
            if let Err(e) = seam_input::warp_cursor(update.x, update.y) {
                tracing::warn!("could not place the returning pointer: {e}");
            }
            None
        }
    }
}

/// Turn a captured non-motion event into a protocol frame.
#[cfg(target_os = "macos")]
fn to_frame(event: seam_input::macos::Observed, seq: u32) -> Option<seam_proto::Frame> {
    use seam_input::macos::Observed;
    Some(match event {
        Observed::Motion { .. } => return None,
        Observed::Button { button, down } => seam_proto::Frame::Button(seam_proto::ButtonEvent {
            seq,
            button: seam_proto::Button::try_from_u8(button).ok()?,
            press: press(down),
            modifiers: seam_proto::Modifiers::NONE,
        }),
        Observed::Scroll { dx, dy } => seam_proto::Frame::Scroll(seam_proto::ScrollEvent {
            seq,
            dx,
            dy,
            unit: seam_proto::ScrollUnit::Detent,
            end_of_gesture: false,
        }),
        Observed::Key { text, physical, modifiers, down } => {
            seam_proto::Frame::Key(seam_proto::KeyEvent {
                seq,
                physical,
                logical: text,
                press: press(down),
                modifiers,
            })
        }
    })
}

#[cfg(target_os = "macos")]
const fn press(down: bool) -> seam_proto::Press {
    if down { seam_proto::Press::Down } else { seam_proto::Press::Up }
}

/// Log one forwarded event.
///
/// Motion is logged at `debug` and everything else at `info`: a 1000 Hz mouse would bury
/// every other line at `info`, whereas a click or a keystroke is exactly what someone
/// watching the log wants to see. `SEAM_LOG=seam=debug` shows movement too.
#[cfg(target_os = "macos")]
fn log_event(frame: &seam_proto::Frame) {
    match frame {
        seam_proto::Frame::Motion(m) => {
            let (x, y) = m.cursor.to_px();
            tracing::debug!(x, y, seq = m.seq, "pointer");
        }
        seam_proto::Frame::Button(b) => {
            tracing::info!(button = ?b.button, down = b.press.is_down(), "button");
        }
        seam_proto::Frame::Scroll(s) => tracing::info!(dx = s.dx, dy = s.dy, "scroll"),
        seam_proto::Frame::Key(k) => {
            tracing::info!(
                text = k.logical.as_str(),
                physical = k.physical.0,
                modifiers = ?k.modifiers,
                down = k.press.is_down(),
                "key"
            );
        }
        _ => {}
    }
}

/// Receive buttons, scroll and keystrokes, which travel reliably.
async fn receive_reliable(
    link: Arc<Link>,
    geometry: Geometry,
    clipboard: Clipboard,
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
) {
    let peer = link.peer_id();
    loop {
        match link.recv_reliable().await {
            Ok(seam_proto::Frame::Key(k)) => {
                // The policy that makes mismatched layouts work, finally in use. A command
                // chord replays the physical key, so Cmd+C stays copy on any layout; plain
                // text replays the glyph, so `@` typed as Option+L on a German Mac arrives
                // as `@` rather than as `l`.
                let outcome = match seam_proto::resolve_replay(
                    seam_proto::LayoutPolicy::Auto,
                    k.physical,
                    k.logical,
                    k.modifiers,
                ) {
                    seam_proto::Replay::Physical(key) => {
                        seam_input::inject_key(key.0, k.press.is_down())
                    }
                    seam_proto::Replay::Text(text) => {
                        seam_input::inject_text(text.as_str(), k.press.is_down())
                    }
                };
                if let Err(e) = outcome {
                    tracing::warn!(%peer, physical = k.physical.0, "could not send key: {e}");
                }
            }
            Ok(seam_proto::Frame::Button(b)) => {
                apply_button(peer, b.button.to_u8(), b.press.is_down());
            }
            Ok(seam_proto::Frame::Scroll(sc)) => {
                if let Err(e) = seam_input::inject_scroll(sc.dx, sc.dy) {
                    tracing::warn!(%peer, "could not scroll: {e}");
                }
            }
            Ok(seam_proto::Frame::ClipboardText { generation, text, .. }) => {
                let mut state = clipboard.lock().await;
                if state.already_applied(peer, generation) {
                    // An echo, or something older than what we already have.
                    tracing::debug!(%peer, generation, "clipboard text ignored as an echo or older");
                    continue;
                }
                match seam_input::clipboard::write_text(&text) {
                    Ok(()) => {
                        tracing::info!(%peer, chars = text.chars().count(), "clipboard received");
                        state.record(peer, generation);
                        // Remember what we just wrote, so the poller does not see it as a
                        // local change and send it straight back.
                        state.last_seen = Some(text.clone());

                        // Relay to every *other* peer. Machines connect in a star through
                        // whichever one they dialled, so without this a copy on one leaf
                        // reaches the centre and stops there.
                        //
                        // Reissued under THIS machine's counter, not passed through. A
                        // leaf tracks generations per link, so a passed-through value
                        // interleaved the origin's counter with this machine's own under
                        // one key — and whichever counter happened to be ahead silently
                        // blocked the other's next copy at the far end. Echo is already
                        // impossible structurally: the origin is skipped below.
                        state.generation += 1;
                        let relay = seam_proto::Frame::ClipboardText {
                            seq: 0,
                            generation: state.generation,
                            text,
                        };
                        drop(state);
                        for other in links.lock().await.iter() {
                            if other.peer_id() == peer {
                                continue;
                            }
                            if let Err(e) = other.send_reliable(&relay).await {
                                tracing::warn!(peer = %other.peer_id(), "could not relay the clipboard: {e}");
                            }
                        }
                    }
                    Err(e) => tracing::warn!(%peer, "could not set the clipboard: {e}"),
                }
            }
            Ok(seam_proto::Frame::ClipboardFiles { generation, entries, .. }) => {
                apply_clipboard_files(peer, generation, entries, &clipboard, &links).await;
            }
            Ok(seam_proto::Frame::ClipboardImage { generation, width, height, rgba, .. }) => {
                apply_clipboard_image(peer, generation, width, height, rgba, &clipboard, &links)
                    .await;
            }
            Ok(seam_proto::Frame::Hello(hello)) => {
                if hello_speaks_another_protocol(&link, peer, hello.version) {
                    return;
                }
                let (w, h) = (
                    i32::try_from(hello.width).unwrap_or(0),
                    i32::try_from(hello.height).unwrap_or(0),
                );
                if w > 0 && h > 0 {
                    tracing::info!(%peer, w, h, "peer reported its screen size");
                    geometry.lock().await.insert(peer, (w, h));
                    if let Some((_, version)) = hello.name.split_once('\t')
                        && let Ok(mut versions) = UI_VERSIONS.lock()
                    {
                        versions.retain(|(p, _)| *p != peer);
                        versions.push((peer, version.to_owned()));
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                // Loud, and fatal to the link. This reader is the only thing that
                // delivers buttons, keys and clipboard from the peer: letting the
                // connection outlive it produced a link that looked healthy while
                // clicks poured into a stream nobody read — and the sender's machine
                // eventually wedged its own input on the backed-up send. If the reader
                // ends, the link ends, both sides notice, and the sweep re-dials.
                tracing::info!(%peer, "reliable stream ended: {e} — closing this link so it can heal");
                link.close("reliable stream ended");
                return;
            }
        }
    }
}

/// Capture this machine's pointer and send it to every connected peer.
///
/// **Mirror mode.** The capture is listen-only, so the local pointer keeps moving too:
/// this proves the whole path end to end without any code being able to suppress input,
/// which is the failure that can freeze a Mac until it is rebooted.
#[expect(clippy::too_many_lines, reason = "one event loop; splitting it hides the ordering")]
fn start_pointer_forwarding(
    links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>,
    geometry: Geometry,
    dir: std::path::PathBuf,
) {
    #[cfg(target_os = "macos")]
    {
        let observed = match seam_input::macos::observe_pointer() {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!("not forwarding this machine's pointer: {e}");
                return;
            }
        };
        tracing::info!("watching this machine's input; peers take it when the pointer crosses an edge");

        // The blocking receiver lives on its own thread so nothing on the async runtime
        // can stall the input path.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<seam_input::macos::Observed>(512);
        std::thread::spawn(move || {
            while let Ok(event) = observed.recv() {
                let _ = tx.try_send(event);
            }
        });

        let desktop = seam_input::desktop().ok();
        let (local_w, local_h) = desktop
            .as_ref()
            .map_or((1920, 1080), |d| (d.bounding_box().width, d.bounding_box().height));

        tokio::spawn(async move {
            use focus::Focus;
            use seam_input::macos::Observed;

            let mut graph = focus::Graph::new(local_w, local_h);
            let mut known: Vec<seam_proto::PeerId> = Vec::new();
            let mut buf = Vec::with_capacity(64);
            let mut seq: u32 = 0;
            // Holds the cursor still on this machine while a peer owns the pointer.
            let mut detached: Option<seam_input::macos::CursorGuard> = None;
            // Which peer currently holds the pointer, for leave announcements.
            let mut holder: Option<seam_proto::PeerId> = None;
            // Where the local cursor is held while a peer owns the pointer.
            let mut parked: Option<(i32, i32)> = None;
            // Rate limit for the drift warning below, so a moving cursor does not turn
            // the log into noise at event rate.
            let mut last_drift_warn: Option<std::time::Instant> = None;

            // The loop wakes on input — and once a second regardless. Rescue used to be
            // event-driven only: a laptop that died HOLDING the pointer left the person
            // staring at a dead cursor, and input only came home when they happened to
            // move the mouse — measured live at 34 seconds of an invisible pointer,
            // because who keeps wiggling a mouse that is visibly dead? The idle tick
            // runs the same reconciliation with no event required.
            let mut idle = tokio::time::interval(Duration::from_secs(1));
            idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let event = tokio::select! {
                    received = rx.recv() => match received {
                        Some(event) => Some(event),
                        None => break,
                    },
                    _ = idle.tick() => None,
                };
                if event.is_some() {
                    if let Ok(mut last) = LAST_INPUT.lock() {
                        *last = Some(std::time::Instant::now());
                    }
                    seq = seq.wrapping_add(1);
                }

                sync_peers(&links, &mut graph, &mut known, &geometry, &dir).await;

                // Safety net. `sync_peers` can hand focus back on its own — a peer that
                // disappears while holding the pointer is sent home immediately — and that
                // path produces no focus *transition* for the handover code below to see.
                // Without this check the Mac would keep withholding its own input from
                // itself, with no way back. Fail open, always.
                if graph.focus() == Focus::Local && seam_input::macos::is_suppressing_local() {
                    tracing::info!("input returned to this machine");
                    if let Ok(mut f) = UI_FOCUS.lock() {
                        *f = None;
                    }
                    seam_input::macos::set_suppress_local(false);
                    detached = None;
                }

                // The UI's release button: send the pointer home before anything else.
                if UI_RELEASE.swap(false, std::sync::atomic::Ordering::Relaxed)
                    && graph.focus() != Focus::Local
                {
                    let u = graph.force_home();
                    tracing::info!("input released to this machine from the fleet page");
                    if let Some(lost) = holder {
                        let links = Arc::clone(&links);
                        tokio::spawn(async move {
                            let peers = links.lock().await;
                            if let Some(link) = peers.iter().find(|l| l.peer_id() == lost) {
                                let _ = link
                                    .send_reliable(&seam_proto::Frame::Leave { seq: 0 })
                                    .await;
                            }
                        });
                    }
                    holder = None;
                    detached = handover(u, detached);
                }

                // A tick has no event to forward; its work — reconciliation and the
                // safety nets above — is done.
                let Some(event) = event else { continue };

                // Movement decides ownership; everything else follows whoever owns it.
                let update = match event {
                    Observed::Motion { x, y, dx, dy } => {
                        // While input is local the OS cursor is the truth; once it has
                        // left, the cursor is frozen and only movement means anything.
                        graph.sync_local_cursor(x, y);
                        Some(graph.apply_motion(dx, dy))
                    }
                    _ => None,
                };

                if let Some(u) = update
                    && u.changed
                {
                    // Tell the peer that LOST the pointer, so it can hide its cursor.
                    // The gaining peer needs no announcement — input starts arriving,
                    // which is unambiguous — but a losing peer sees only silence, and
                    // silence is indistinguishable from a user who stopped moving.
                    let now_holding = match u.focus {
                        Focus::Remote(p) => Some(p),
                        Focus::Local => None,
                    };
                    if let Some(lost) = holder
                        && now_holding != Some(lost)
                    {
                        let links = Arc::clone(&links);
                        tokio::spawn(async move {
                            let peers = links.lock().await;
                            if let Some(link) = peers.iter().find(|l| l.peer_id() == lost)
                                && let Err(e) = link
                                    .send_reliable(&seam_proto::Frame::Leave { seq: 0 })
                                    .await
                            {
                                tracing::debug!("could not send leave: {e}");
                            }
                        });
                    }
                    holder = now_holding;
                    detached = handover(u, detached);
                    match u.focus {
                        // Remember where the cursor was left, and hold it there. Logged,
                        // because if this read fails the pin and the drift check below
                        // never run at all - and a silently vacuous check would look
                        // exactly like a clean pass.
                        Focus::Remote(_) if parked.is_none() => {
                            match seam_input::cursor_position() {
                                Ok((px, py)) => {
                                    // A crossing happens AT an edge, so the raw position
                                    // is often the screen's last pixel row — and a cursor
                                    // returned to a screen seam is, from a chair, not
                                    // there at all: "mouse not shown even after the peer
                                    // disconnected" was a cursor parked at (59, 1080) on
                                    // a 1080-tall display. Hold it a visible distance in.
                                    let px = px.clamp(24, (local_w - 25).max(24));
                                    let py = py.clamp(24, (local_h - 25).max(24));
                                    tracing::info!(
                                        x = px,
                                        y = py,
                                        "holding the local cursor here while the pointer is away"
                                    );
                                    parked = Some((px, py));
                                }
                                Err(e) => tracing::warn!(
                                    "cannot pin the local cursor; its position is unreadable: {e}"
                                ),
                            }
                        }
                        Focus::Local => parked = None,
                        Focus::Remote(_) => {}
                    }
                }

                // Pin the local cursor while a peer owns the pointer.
                //
                // `CGAssociateMouseAndMouseCursorPosition(0)` returns success from a daemon
                // but does not take effect without foreground status, so the cursor kept
                // tracking the mouse on this machine even though every event was being
                // withheld correctly. Warping it back on each movement does not depend on
                // foreground status and is what Barrier does.
                //
                // This is safe now in a way it was not before: an earlier attempt
                // (`park_cursor`) fed back, because the graph adopted the warped position
                // and immediately re-crossed the boundary. `Graph::sync_local_cursor`
                // returns early while focus is remote, so the warp cannot reach the graph.
                // Warping also generates no events, so it cannot reach the tap either.
                // Belt and braces: check the graph, not just the stored point. A stale
                // `parked` while focus is local would pin the cursor and leave this
                // machine unusable, which is the exact failure the earlier `park_cursor`
                // caused twice. Two independent conditions must agree before any warp.
                if graph.focus() != Focus::Local
                    && let (Some((px, py)), true) =
                        (parked, matches!(event, Observed::Motion { .. }))
                {
                    // Measure before correcting. If the OS cursor is found away from
                    // where it was parked, the detach is NOT holding in this process,
                    // whatever its return value claimed - and that one line separates
                    // the two remaining theories: real movement (detach broken, fix by
                    // re-asserting it) versus movement the user can see but the OS
                    // denies (a visibility problem, needing a different fix entirely).
                    // Every bug fixed in this project was fixed by a line like this.
                    if let Ok((cx, cy)) = seam_input::cursor_position()
                        && (cx - px).abs().max((cy - py).abs()) > 4
                        && last_drift_warn.is_none_or(|t| t.elapsed() > Duration::from_secs(1))
                    {
                        tracing::warn!(
                            parked = ?(px, py),
                            cursor = ?(cx, cy),
                            "local cursor moved while detached - re-asserting the detach"
                        );
                        last_drift_warn = Some(std::time::Instant::now());
                    }
                    if let Err(e) = seam_input::warp_cursor(px, py) {
                        tracing::debug!("could not hold the local cursor still: {e}");
                    }
                    // Warping is suspected of re-associating the cursor with the mouse,
                    // which would make the pin itself the thing that unfreezes the
                    // cursor: each event moves it, the next pin yanks it back, and the
                    // user sees flicker. One extra call per event makes that impossible
                    // rather than arguable.
                    seam_input::macos::reassert_detach();
                    // Visibility is global OS state: any foreground app or system banner
                    // can show the cursor again mid-session, and did — the field report
                    // was the arrow reappearing until the pointer came home. Watch and
                    // re-hide, and say so in the log instead of leaving it to be noticed.
                    if seam_input::macos::rehide_if_visible()
                        && last_drift_warn.is_none_or(|t| t.elapsed() > Duration::from_secs(1))
                    {
                        tracing::info!(
                            "the cursor had become visible while a peer holds the \
                             pointer — hidden again"
                        );
                        last_drift_warn = Some(std::time::Instant::now());
                    }
                }

                // Log the captured event and the resulting focus *before* deciding
                // whether to forward it. Logging only after the "focus is local" branch
                // meant that when something went wrong — no crossing, no return — the log
                // was completely silent, which is the opposite of what a diagnostic is
                // for. `SEAM_LOG=seam=debug` now shows every event and where it went.
                if let Some(u) = update {
                    tracing::debug!(
                        x = u.x,
                        y = u.y,
                        focus = ?u.focus,
                        changed = u.changed,
                        suppressing = seam_input::macos::is_suppressing_local(),
                        "pointer"
                    );
                }

                // A machine switched off in the UI is skipped without unpairing: the link
                // and its clipboard stay, only input stops. Checked here rather than at
                // placement so toggling takes effect immediately, mid-session.
                if let Focus::Remote(peer) = graph.focus()
                    && UI_DISABLED.lock().is_ok_and(|off| off.contains(&peer))
                {
                    let u = graph.force_home();
                    detached = handover(u, detached);
                    holder = None;
                    continue;
                }

                let Some(target) = (match graph.focus() {
                    Focus::Local => None,
                    Focus::Remote(p) => Some(p),
                }) else {
                    // Local machine owns input: forward nothing at all. This is what makes
                    // it a KVM rather than a mirror.
                    continue;
                };

                let frame = match event {
                    Observed::Motion { .. } => {
                        let u = update.unwrap_or_else(|| graph.apply_motion(0, 0));
                        seam_proto::Frame::Motion(seam_proto::Motion {
                            seq,
                            cursor: seam_proto::Point::from_px(u.x, u.y),
                            travel_x: u.x,
                            travel_y: u.y,
                        })
                    }
                    other => match to_frame(other, seq) {
                        Some(f) => f,
                        None => continue,
                    },
                };
                log_event(&frame);

                // Clone out of the lock before awaiting anything: holding the fleet's
                // link list across a blocked send would freeze every other user of it.
                let link = {
                    let peers = links.lock().await;
                    match peers.iter().find(|l| l.peer_id() == target) {
                        Some(link) => Arc::clone(link),
                        None => continue,
                    }
                };
                let result = if frame.is_datagram_safe() {
                    link.send_datagram(&frame, &mut buf)
                } else {
                    // Bounded, because this loop owns this machine's own input. A peer
                    // whose reliable stream is not being drained exerts backpressure
                    // here, and an unbounded await turned one sick client into a server
                    // whose own mouse and keyboard were dead until that client
                    // disconnected. Two seconds without progress means the peer is not
                    // accepting input; close the link and let the sweep heal it.
                    let Ok(sent) = tokio::time::timeout(
                        Duration::from_secs(2),
                        link.send_reliable(&frame),
                    )
                    .await
                    else {
                        tracing::warn!(
                            peer = %target,
                            "an input send made no progress for two seconds; \
                             closing the link so it can heal"
                        );
                        link.close("input send stalled");
                        continue;
                    };
                    sent
                };
                if let Err(e) = result {
                    tracing::warn!(peer = %target, "could not forward input: {e}");
                }
            }
            // Whatever ends this loop, this machine gets its own input back.
            seam_input::macos::set_suppress_local(false);
            drop(detached);
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (links, geometry);
        tracing::info!("this machine receives input only; capture is not built for it yet");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_restarted_peer_is_heard_again() {
        // A watermark outlives the process it described: the laptop restarted, began
        // counting from one again, and every copy it made was then "older" than the
        // generation remembered from its previous life — dropped at the gate with no
        // receipt logged, nothing applied, nothing relayed. Forgetting the peer when
        // its link re-registers is what lets the reborn machine speak.
        let mut state = ClipboardState::default();
        let peer = seam_proto::PeerId([7; 16]);
        state.record(peer, 40);
        assert!(state.already_applied(peer, 1), "the old life's watermark blocks the reborn counter");
        state.forget_peer(peer);
        assert!(!state.already_applied(peer, 1), "after the link re-registers, generation 1 is heard");
    }

    #[test]
    fn pixels_survive_the_wire_and_lies_about_size_do_not() {
        // 100x50 of gradient-ish pixels: representative of a screenshot's compressibility.
        let (w, h) = (100u32, 50u32);
        let raw: Vec<u8> = (0..(w * h * 4)).map(|i| (i % 251) as u8).collect();
        let wire = deflate_pixels(&raw);
        assert!(wire.len() < raw.len(), "screenshot-like pixels must shrink");
        assert_eq!(inflate_pixels(&wire, w, h).as_deref(), Some(raw.as_slice()));
        // A frame whose dimensions do not match its pixels is corruption, not a guess.
        assert_eq!(inflate_pixels(&wire, w, h + 1), None);
        assert_eq!(inflate_pixels(&wire[..wire.len() / 2], w, h), None);
    }

    #[test]
    fn an_echo_is_recognised_and_newer_generations_pass() {
        let mut state = ClipboardState::default();
        let peer = seam_proto::PeerId([7; 16]);
        state.record(peer, 5);
        assert!(state.already_applied(peer, 5), "the same generation is an echo");
        assert!(state.already_applied(peer, 4), "an older one is stale");
        assert!(!state.already_applied(peer, 6), "the next copy must pass");
    }

    #[test]
    fn every_command_works_without_a_single_configuration_value() {
        // Goal Z1: nothing here may require an address, a port, a role or a config file.
        // `pair <peer>` takes a name, and `forget` takes a peer, but neither is a setting.
        for args in [
            vec!["seam", "doctor"],
            vec!["seam", "discover"],
            vec!["seam", "pair"],
            vec!["seam", "peers"],
            vec!["seam", "run"],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?} should need no arguments");
        }
    }

    #[test]
    fn the_port_defaults_to_a_stable_number() {
        let Some(Command::Run { port, connect, .. }) =
            Cli::try_parse_from(["seam", "run"]).unwrap().command
        else {
            panic!("expected run");
        };
        // Stable, not OS-chosen. A machine that takes a random port each start cannot
        // be dialled by a peer that was told its address once — and the fleet then shows
        // "no machines connected" on every screen with nothing obviously wrong.
        assert_eq!(port, DEFAULT_PORT, "the listening port must not move between runs");
        assert!(connect.is_empty(), "dialling out is opt-in: discovery finds paired peers");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod handover_state {
    //! The suppression flag across sequences of handovers.
    //!
    //! "The pointer will not come back" and "the pointer came back but the Mac stays
    //! muted" are indistinguishable from the user's chair, and only the second is about
    //! this flag. It is a process-wide global, so it is the one piece of state that can
    //! survive a wrong sequence and leave the machine unusable — which is exactly the
    //! regression that shipped when a refactor changed a drop order.

    use super::*;
    use focus::{Focus, Update};

    fn to(focus: Focus) -> Update {
        Update { focus, changed: true, x: 100, y: 100 }
    }

    /// Serialised: the flag is global, so these cannot run concurrently.
    fn with_clean_state(body: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        seam_input::macos::set_suppress_local(false);
        body();
        seam_input::macos::set_suppress_local(false);
        seam_input::release_input();
    }

    #[test]
    fn leaving_mutes_this_machine_and_returning_unmutes_it() {
        with_clean_state(|| {
            let guard = handover(to(Focus::Remote(seam_proto::PeerId([1; 16]))), None);
            assert!(seam_input::macos::is_suppressing_local(), "should be muted while away");

            let guard = handover(to(Focus::Local), guard);
            assert!(!seam_input::macos::is_suppressing_local(), "should respond again");
            assert!(guard.is_none());
        });
    }

    #[test]
    fn moving_between_two_peers_keeps_this_machine_muted() {
        // The regression that shipped: the incoming guard was dropped *after* the new one
        // was made, and its Drop cleared suppression — so hopping peers silently switched
        // this machine's input back on while a peer still owned the pointer.
        with_clean_state(|| {
            let guard = handover(to(Focus::Remote(seam_proto::PeerId([1; 16]))), None);
            assert!(seam_input::macos::is_suppressing_local());

            let guard = handover(to(Focus::Remote(seam_proto::PeerId([2; 16]))), guard);
            assert!(
                seam_input::macos::is_suppressing_local(),
                "hopping between peers must not unmute this machine"
            );

            handover(to(Focus::Local), guard);
            assert!(!seam_input::macos::is_suppressing_local());
        });
    }

    #[test]
    fn repeated_round_trips_always_end_unmuted() {
        // A one-way leak passes a single trip and strands the machine on a later one.
        with_clean_state(|| {
            let mut guard = None;
            for trip in 0..5 {
                guard = handover(to(Focus::Remote(seam_proto::PeerId([1; 16]))), guard);
                assert!(seam_input::macos::is_suppressing_local(), "trip {trip}: not muted");

                guard = handover(to(Focus::Local), guard);
                assert!(
                    !seam_input::macos::is_suppressing_local(),
                    "trip {trip}: this machine was left muted"
                );
            }
            assert!(guard.is_none());
        });
    }

    #[test]
    fn returning_twice_is_harmless() {
        // sync_peers can hand focus back on its own, so `Local` can arrive without a
        // matching `Remote`. It must never leave the machine muted.
        with_clean_state(|| {
            let guard = handover(to(Focus::Local), None);
            assert!(!seam_input::macos::is_suppressing_local());
            handover(to(Focus::Local), guard);
            assert!(!seam_input::macos::is_suppressing_local());
        });
    }
}
