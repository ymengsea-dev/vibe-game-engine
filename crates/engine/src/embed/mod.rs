//! Running a game inside Studio's Scene View: the engine side of the Play
//! button.
//!
//! Studio starts the game's normal program (the Lua player or a Rust
//! game's binary) with [`EMBED_ENV`] set to `<port>:<token>`. [`run_game`]
//! notices and, instead of opening a window, connects to Studio on
//! `127.0.0.1:<port>`, proves it is the process Studio launched with the
//! token, and renders headless. Each frame is read back and sent to
//! Studio; Studio sends input, resizes, Pause/Resume/Step and Stop (see
//! [`protocol`]).
//!
//! The game loop is the same [`GameRunner`](crate::app) a window drives,
//! fed the same [`PlatformEvent`](engine_platform::PlatformEvent)s, so a
//! game behaves identically in Play and Run with no code of its own.
//!
//! [`run_game`]: crate::app::run_game

mod driver;
pub mod protocol;

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use engine_renderer::GpuContext;

use crate::app::{Game, GameError, GameRunner};
use protocol::{PROTOCOL_VERSION, ProtocolError, ToGame, ToStudio};

/// Environment variable Studio sets to start a game in its Scene View:
/// `<port>:<token>`.
pub const EMBED_ENV: &str = "VGE_EMBED";

/// How long the game waits for Studio to accept the connection and send
/// the first size.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Time between frames: the preview runs at up to 60 frames per second.
pub const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// Game time one Step advances.
pub const STEP_SECONDS: f32 = 1.0 / 60.0;

/// Where to find Studio, from [`EMBED_ENV`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedTarget {
    /// Studio's listening port on `127.0.0.1`.
    pub port: u16,
    /// The secret Studio expects in [`ToStudio::Hello`].
    pub token: String,
}

impl EmbedTarget {
    /// Parses an [`EMBED_ENV`] value.
    ///
    /// # Errors
    ///
    /// [`GameError::Embed`] when the value is not `<port>:<token>` with a
    /// nonzero port and a non-empty token.
    pub fn parse(value: &str) -> Result<Self, GameError> {
        let malformed = || GameError::Embed(format!("{EMBED_ENV} must be <port>:<token>"));
        let (port, token) = value.split_once(':').ok_or_else(malformed)?;
        let port: u16 = port.parse().map_err(|_| malformed())?;
        if port == 0 || token.is_empty() {
            return Err(malformed());
        }
        Ok(Self {
            port,
            token: token.to_owned(),
        })
    }

    /// The [`EMBED_ENV`] value that [`EmbedTarget::parse`] reads back.
    pub fn to_env_value(&self) -> String {
        format!("{}:{}", self.port, self.token)
    }

    /// The target from this process's environment, if Studio set one.
    pub(crate) fn from_env() -> Result<Option<Self>, GameError> {
        match std::env::var(EMBED_ENV) {
            Ok(value) => Self::parse(&value).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(GameError::Embed(format!("{EMBED_ENV} is not valid text")))
            }
        }
    }
}

fn embed_error(context: &str, err: impl std::fmt::Display) -> GameError {
    GameError::Embed(format!("{context}: {err}"))
}

/// Connects to Studio, renders headless at the size it asks for, and runs
/// the game until Studio stops it, the game exits, or the connection
/// closes.
pub(crate) fn run<G: Game>(
    runner: &mut GameRunner<G>,
    target: &EmbedTarget,
) -> Result<(), GameError> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut stream = TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT)
        .map_err(|err| embed_error("could not reach Studio", err))?;
    stream
        .set_nodelay(true)
        .map_err(|err| embed_error("could not configure the connection", err))?;
    protocol::write_to_studio(
        &mut stream,
        &ToStudio::Hello {
            protocol: PROTOCOL_VERSION,
            token: target.token.clone(),
        },
    )
    .map_err(|err| embed_error("could not greet Studio", err))?;

    let Some((width, height)) = wait_for_size(&mut stream)? else {
        // Studio stopped the preview before it started.
        return Ok(());
    };
    tracing::info!(width, height, "running inside Studio's Scene View");
    let gpu = GpuContext::headless(width, height)?;
    runner.start_with_gpu(gpu, width, height);

    let mut link = TcpLink::new(stream)?;
    driver::drive(runner, &mut link, FRAME_INTERVAL)
        .map_err(|err| embed_error("lost the connection to Studio", err))
}

/// Reads until Studio sends the first [`ToGame::Resize`]; `None` if it
/// sends [`ToGame::Stop`] first. Other messages before then are ignored.
fn wait_for_size(stream: &mut TcpStream) -> Result<Option<(u32, u32)>, GameError> {
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|err| embed_error("could not configure the connection", err))?;
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let size = loop {
        if Instant::now() >= deadline {
            return Err(GameError::Embed("Studio sent no size in time".to_owned()));
        }
        match protocol::read_to_game(stream) {
            Ok(ToGame::Resize { width, height }) if driver::valid_size(width, height) => {
                break Some((width, height));
            }
            Ok(ToGame::Stop) => break None,
            Ok(_) => {}
            Err(err) => return Err(embed_error("handshake with Studio failed", err)),
        }
    };
    stream
        .set_read_timeout(None)
        .map_err(|err| embed_error("could not configure the connection", err))?;
    Ok(size)
}

/// One step of reading from Studio.
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming {
    /// A message arrived.
    Message(ToGame),
    /// Nothing waiting right now.
    Empty,
    /// Studio closed the connection or sent something unreadable.
    Closed,
}

/// The connection to Studio, as the frame loop sees it.
pub(crate) trait Link {
    /// The next message from Studio, without blocking.
    fn poll(&mut self) -> Incoming;
    /// Sends a message to Studio.
    fn send(&mut self, message: &ToStudio) -> std::io::Result<()>;
}

/// [`Link`] over TCP: a reader thread decodes Studio's messages into a
/// channel, so polling never blocks the game; writes go straight out.
struct TcpLink {
    writer: std::io::BufWriter<TcpStream>,
    messages: Receiver<Result<ToGame, ProtocolError>>,
}

impl TcpLink {
    fn new(stream: TcpStream) -> Result<Self, GameError> {
        let mut reader = stream
            .try_clone()
            .map_err(|err| embed_error("could not read from Studio", err))?;
        let (sender, messages) = mpsc::sync_channel(256);
        std::thread::Builder::new()
            .name("vge-embed-reader".to_owned())
            .spawn(move || {
                loop {
                    let message = protocol::read_to_game(&mut reader);
                    let failed = message.is_err();
                    if sender.send(message).is_err() || failed {
                        break;
                    }
                }
            })
            .map_err(|err| embed_error("could not start the Studio reader", err))?;
        Ok(Self {
            writer: std::io::BufWriter::new(stream),
            messages,
        })
    }
}

impl Link for TcpLink {
    fn poll(&mut self) -> Incoming {
        match self.messages.try_recv() {
            Ok(Ok(message)) => Incoming::Message(message),
            Ok(Err(err)) => {
                tracing::info!(reason = %err, "Studio closed the preview connection");
                Incoming::Closed
            }
            Err(TryRecvError::Empty) => Incoming::Empty,
            Err(TryRecvError::Disconnected) => Incoming::Closed,
        }
    }

    fn send(&mut self, message: &ToStudio) -> std::io::Result<()> {
        protocol::write_to_studio(&mut self.writer, message)?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_value_round_trips_and_rejects_nonsense() {
        let target = EmbedTarget {
            port: 49_152,
            token: "3f2a".to_owned(),
        };
        assert_eq!(
            EmbedTarget::parse(&target.to_env_value()).expect("parse"),
            target
        );
        for bad in [
            "",
            "49152",
            ":abc",
            "0:abc",
            "70000:abc",
            "49152:",
            "port:abc",
        ] {
            assert!(
                EmbedTarget::parse(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn the_handshake_waits_for_a_size_and_honours_an_early_stop() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("address").port();
        let studio = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            protocol::write_to_game(&mut stream, &ToGame::Pause).expect("noise first");
            protocol::write_to_game(
                &mut stream,
                &ToGame::Resize {
                    width: 0,
                    height: 5,
                },
            )
            .expect("an invalid size is skipped");
            protocol::write_to_game(
                &mut stream,
                &ToGame::Resize {
                    width: 640,
                    height: 360,
                },
            )
            .expect("size");
            let (mut second, _) = listener.accept().expect("accept");
            protocol::write_to_game(&mut second, &ToGame::Stop).expect("stop");
        });
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        assert_eq!(wait_for_size(&mut stream).expect("size"), Some((640, 360)));
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        assert_eq!(wait_for_size(&mut stream).expect("stopped"), None);
        studio.join().expect("studio thread");
    }
}
