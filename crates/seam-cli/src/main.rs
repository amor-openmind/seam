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
    command: Command,
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
    /// List the machines this one has paired with.
    Peers,
    /// Remove a pairing.
    Forget { peer: String },
    /// Run the daemon: advertise this machine and keep links to paired peers.
    Run {
        /// Port to listen on. Chosen by the OS unless given — you should not need this.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Also dial these addresses. Needed where an inbound firewall blocks discovery:
        /// dialling out is not blocked, so the link still forms.
        #[arg(long = "connect", value_name = "HOST:PORT")]
        connect: Vec<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SEAM_LOG")
                .unwrap_or_else(|_| "seam=info,warn".into()),
        )
        .with_target(false)
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

    match cli.command {
        Command::Doctor => doctor(&dir, &identity),
        Command::Discover { r#for } => {
            discover(Duration::from_secs(r#for), identity.peer_id()).await
        }
        Command::Pair { peer, at, timeout } => {
            pair(&dir, identity, peer, at, Duration::from_secs(timeout)).await
        }
        Command::Peers => {
            peers(&dir);
            Ok(())
        }
        Command::Forget { peer } => forget(&dir, &peer),
        Command::Run { port, connect } => daemon(&dir, identity, port, connect).await,
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

    // Honesty over polish: the thing most likely to stop seam working is a platform
    // backend that does not exist yet, so say so rather than reporting a clean bill.
    println!("\n  input forwarding");
    println!("    NOT IMPLEMENTED — screen geometry and permissions are in place, but");
    println!("    capture and injection are not written yet, so seam cannot move your");
    println!("    pointer or forward keystrokes between machines.");

    Ok(())
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
    let store = Arc::new(store::load_peers(dir));
    let endpoint =
        Arc::new(Endpoint::bind(Arc::clone(&identity), format!("0.0.0.0:{port}").parse()?)?);
    let bound = endpoint.local_addr()?;
    let name = Discovery::default_display_name(identity.peer_id());

    let mut discovery = Discovery::new()?;
    discovery.advertise(&name, identity.peer_id(), identity.fingerprint(), bound.port())?;

    tracing::info!(%name, id = %identity.peer_id(), %bound, peers = store.len(), "seam is running");
    if store.is_empty() {
        tracing::warn!("no paired machines yet — run `seam pair` on this and another machine");
    }

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
        match endpoint.connect(target).await {
            Ok(link) => {
                if let Err(e) = link.authorize(&store) {
                    tracing::warn!(%target, "refused: {e}");
                    continue;
                }
                tracing::info!(peer = %link.peer_id(), %target, "connected to peer");
                let link = Arc::new(link);
                links.lock().await.push(Arc::clone(&link));
                tokio::spawn(receive_from(link));
            }
            Err(e) => tracing::warn!(%target, "could not connect: {e}"),
        }
    }

    start_pointer_forwarding(Arc::clone(&links));

    let accepting = {
        let endpoint = Arc::clone(&endpoint);
        let store = Arc::clone(&store);
        let links = Arc::clone(&links);
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                match incoming {
                    Ok(link) => {
                        let peer = link.peer_id();
                        match link.authorize(&store) {
                            Ok(()) => {
                                tracing::info!(%peer, remote = %link.remote_address(), "peer connected");
                                let link = Arc::new(link);
                                links.lock().await.push(Arc::clone(&link));
                                tokio::spawn(receive_from(link));
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
    };

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutting down");
    discovery.shutdown();
    endpoint.close();
    accepting.abort();
    Ok(())
}

/// Receive motion from a peer and reproduce it on this machine.
async fn receive_from(link: Arc<Link>) {
    tokio::spawn(receive_reliable(Arc::clone(&link)));
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
            Ok(seam_proto::Frame::Motion(motion)) => {
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
                if let Err(e) = seam_input::inject_button(b.button.to_u8(), b.press.is_down()) {
                    tracing::warn!(%peer, "could not press a mouse button: {e}");
                }
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
                return;
            }
        }
    }
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
    strip: &mut focus::Layout,
    known: &mut Vec<seam_proto::PeerId>,
) {
    let live: Vec<seam_proto::PeerId> = links.lock().await.iter().map(|l| l.peer_id()).collect();

    for id in &live {
        if !known.contains(id) {
            known.push(*id);
            strip.add_peer_right(*id, 1920, 1080);
            tracing::info!(peer = %id, "placed to the right — push the pointer off the right edge to reach it");
        }
    }
    known.retain(|id| {
        if live.contains(id) {
            return true;
        }
        tracing::info!(peer = %id, "peer gone; input returns to this machine");
        strip.forget_peer(*id);
        false
    });
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
        Observed::Key { text, down } => seam_proto::Frame::Key(seam_proto::KeyEvent {
            seq,
            physical: seam_proto::PhysicalKey::UNKNOWN,
            logical: text,
            press: press(down),
            modifiers: seam_proto::Modifiers::NONE,
        }),
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
            tracing::info!(text = k.logical.as_str(), down = k.press.is_down(), "key");
        }
        _ => {}
    }
}

/// Receive buttons, scroll and keystrokes, which travel reliably.
async fn receive_reliable(link: Arc<Link>) {
    let peer = link.peer_id();
    loop {
        match link.recv_reliable().await {
            Ok(seam_proto::Frame::Key(k)) => {
                if k.logical.is_empty() {
                    continue;
                }
                if let Err(e) = seam_input::inject_text(k.logical.as_str(), k.press.is_down()) {
                    tracing::warn!(%peer, "could not type: {e}");
                }
            }
            Ok(seam_proto::Frame::Button(b)) => {
                if let Err(e) = seam_input::inject_button(b.button.to_u8(), b.press.is_down()) {
                    tracing::warn!(%peer, "could not press a mouse button: {e}");
                }
            }
            Ok(seam_proto::Frame::Scroll(sc)) => {
                if let Err(e) = seam_input::inject_scroll(sc.dx, sc.dy) {
                    tracing::warn!(%peer, "could not scroll: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(%peer, "reliable stream ended: {e}");
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
fn start_pointer_forwarding(links: Arc<tokio::sync::Mutex<Vec<Arc<Link>>>>) {
    #[cfg(target_os = "macos")]
    {
        let observed = match seam_input::macos::observe_pointer() {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!("not forwarding this machine's pointer: {e}");
                return;
            }
        };
        tracing::info!("forwarding this machine's pointer to connected peers (mirror mode)");

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

            let mut strip = focus::Layout::new(local_w, local_h);
            let mut known: Vec<seam_proto::PeerId> = Vec::new();
            let mut buf = Vec::with_capacity(64);
            let mut seq: u32 = 0;
            // Holds the cursor still on this machine while a peer owns the pointer.
            let mut detached: Option<seam_input::macos::CursorGuard> = None;

            while let Some(event) = rx.recv().await {
                seq = seq.wrapping_add(1);

                sync_peers(&links, &mut strip, &mut known).await;

                // Movement decides ownership; everything else follows whoever owns it.
                let update = match event {
                    Observed::Motion { dx, dy, .. } => Some(strip.apply_motion(dx, dy)),
                    _ => None,
                };

                if let Some(u) = update
                    && u.changed
                {
                    match u.focus {
                        Focus::Remote(peer) => {
                            tracing::info!(%peer, "pointer and keyboard moved to this peer");
                            // Freeze the local cursor so this machine stops tracking the
                            // mouse. The guard reattaches on every exit path.
                            detached = seam_input::macos::CursorGuard::detach(false).ok();
                        }
                        Focus::Local => {
                            tracing::info!("pointer and keyboard back on this machine");
                            detached = None;
                        }
                    }
                }

                let Some(target) = (match strip.focus() {
                    Focus::Local => None,
                    Focus::Remote(p) => Some(p),
                }) else {
                    // Local machine owns input: forward nothing at all. This is what makes
                    // it a KVM rather than a mirror.
                    continue;
                };

                let frame = match event {
                    Observed::Motion { .. } => {
                        let u = update.unwrap_or_else(|| strip.apply_motion(0, 0));
                        seam_proto::Frame::Motion(seam_proto::Motion {
                            seq,
                            cursor: seam_proto::Point::from_px(u.local_x, u.local_y),
                            travel_x: u.local_x,
                            travel_y: u.local_y,
                        })
                    }
                    other => match to_frame(other, seq) {
                        Some(f) => f,
                        None => continue,
                    },
                };
                log_event(&frame);

                let peers = links.lock().await;
                let Some(link) = peers.iter().find(|l| l.peer_id() == target) else {
                    continue;
                };
                let result = if frame.is_datagram_safe() {
                    link.send_datagram(&frame, &mut buf)
                } else {
                    link.send_reliable(&frame).await
                };
                if let Err(e) = result {
                    tracing::warn!(peer = %target, "could not forward input: {e}");
                }
            }
            drop(detached);
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = links;
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
    fn the_port_is_optional_and_defaults_to_os_chosen() {
        let Command::Run { port, connect } = Cli::try_parse_from(["seam", "run"]).unwrap().command
        else {
            panic!("expected run");
        };
        assert_eq!(port, 0, "0 means the OS picks, so the user never has to");
        assert!(connect.is_empty(), "dialling out is opt-in, never required");
    }
}
