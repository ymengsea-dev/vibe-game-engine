//! Studio's end of a game playing inside the Scene View.
//!
//! Before the game starts, [`EmbeddedSession::listen`] opens a listener on
//! `127.0.0.1` (an ephemeral port) and makes a one-time token; both go to
//! the game in `VGE_EMBED`. [`EmbeddedSession::poll`] accepts exactly one
//! connection, checks the game's `Hello` (protocol version and token),
//! then a reader thread decodes what the game sends. Only the **newest**
//! frame is kept, so a slow editor frame drops game frames instead of
//! queueing them; acknowledgements go through a small channel. The game
//! side is `engine::embed`.

use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::embed::protocol::{self, Frame, PROTOCOL_VERSION, ToGame, ToStudio};
use engine::embed::{EmbedTarget, HANDSHAKE_TIMEOUT};
use engine::platform::WakeHandle;

/// How long the game's first message may take once it has connected.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// Something that happened since the last [`EmbeddedSession::poll`].
#[derive(Debug, PartialEq)]
pub(crate) enum SessionEvent {
    /// The game connected and proved it is the one Studio started.
    Connected,
    /// The game confirmed a pause.
    Paused,
    /// The game confirmed a resume.
    Resumed,
    /// The game wants the mouse locked (`true`) or released (`false`).
    CursorLock(bool),
    /// The connection closed (the game quit, crashed or was stopped).
    Closed,
    /// The preview cannot continue; the message says why, for the user.
    Failed(String),
}

type Latest = Arc<Mutex<Option<Frame>>>;

struct Connection {
    writer: TcpStream,
    acks: Receiver<SessionEvent>,
    latest: Latest,
}

enum Stage {
    /// Waiting for the game. `since` is when the game was launched (the
    /// build before it does not count), `None` until then.
    Listening {
        listener: TcpListener,
        since: Option<Instant>,
    },
    Connected(Connection),
    Ended,
}

/// One Play-in-viewport connection, from listening to closed.
pub(crate) struct EmbeddedSession {
    target: EmbedTarget,
    stage: Stage,
    wake: Option<WakeHandle>,
}

impl EmbeddedSession {
    /// Opens the listener the game will connect to.
    ///
    /// # Errors
    ///
    /// A readable message if no local port could be opened.
    pub(crate) fn listen(wake: Option<WakeHandle>) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
            .map_err(|err| format!("could not open the preview connection: {err}"))?;
        let port = listener
            .local_addr()
            .map_err(|err| format!("could not open the preview connection: {err}"))?
            .port();
        Ok(Self {
            target: EmbedTarget {
                port,
                token: uuid::Uuid::new_v4().simple().to_string(),
            },
            stage: Stage::Listening {
                listener,
                since: None,
            },
            wake,
        })
    }

    /// Starts the connection timeout: the game process has launched.
    pub(crate) fn game_launched(&mut self) {
        if let Stage::Listening { since, .. } = &mut self.stage {
            since.get_or_insert_with(Instant::now);
        }
    }

    /// The `VGE_EMBED` value to start the game with.
    pub(crate) fn env_value(&self) -> String {
        self.target.to_env_value()
    }

    /// Accepts the game's connection when it arrives and reports what
    /// happened since the last call.
    pub(crate) fn poll(&mut self) -> Vec<SessionEvent> {
        let waiting = match &self.stage {
            Stage::Listening { listener, since } => Some((
                listener.accept(),
                since.map_or(Duration::ZERO, |since| since.elapsed()),
            )),
            Stage::Connected(_) | Stage::Ended => None,
        };
        if let Some((accepted, waited)) = waiting {
            return match accepted {
                Ok((stream, _)) => match self.handshake(stream) {
                    Ok(connection) => {
                        self.stage = Stage::Connected(connection);
                        vec![SessionEvent::Connected]
                    }
                    Err(message) => self.fail(message),
                },
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if waited > HANDSHAKE_TIMEOUT {
                        self.fail(
                            "the game did not start in preview mode. If it is a Rust \
                             project made with an older Studio, use File → Upgrade Rust SDK, \
                             or use Run"
                                .to_owned(),
                        )
                    } else {
                        Vec::new()
                    }
                }
                Err(err) => self.fail(format!("the preview connection failed: {err}")),
            };
        }
        let Stage::Connected(connection) = &mut self.stage else {
            return Vec::new();
        };
        let mut events = Vec::new();
        loop {
            match connection.acks.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    events.push(SessionEvent::Closed);
                    break;
                }
            }
        }
        if events
            .iter()
            .any(|event| matches!(event, SessionEvent::Closed | SessionEvent::Failed(_)))
        {
            self.stage = Stage::Ended;
        }
        events
    }

    /// The newest frame the game sent since the last call, if any.
    pub(crate) fn take_frame(&mut self) -> Option<Frame> {
        match &self.stage {
            Stage::Connected(connection) => match connection.latest.lock() {
                Ok(mut latest) => latest.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            },
            Stage::Listening { .. } | Stage::Ended => None,
        }
    }

    /// Sends `message` to a connected game. Before the connection, or
    /// after it closed, nothing is sent.
    ///
    /// # Errors
    ///
    /// A readable message if the connection broke.
    pub(crate) fn send(&mut self, message: &ToGame) -> Result<(), String> {
        match &mut self.stage {
            Stage::Connected(connection) => {
                protocol::write_to_game(&mut connection.writer, message)
                    .map_err(|err| format!("could not reach the game: {err}"))
            }
            Stage::Listening { .. } | Stage::Ended => Ok(()),
        }
    }

    /// Whether the game has connected and is still connected.
    pub(crate) fn is_connected(&self) -> bool {
        matches!(self.stage, Stage::Connected(_))
    }

    fn fail(&mut self, message: String) -> Vec<SessionEvent> {
        self.stage = Stage::Ended;
        vec![SessionEvent::Failed(message)]
    }

    fn handshake(&self, mut stream: TcpStream) -> Result<Connection, String> {
        let setup = |err: std::io::Error| format!("the preview connection failed: {err}");
        stream.set_nonblocking(false).map_err(setup)?;
        stream.set_nodelay(true).map_err(setup)?;
        stream
            .set_read_timeout(Some(HELLO_TIMEOUT))
            .map_err(setup)?;
        match protocol::read_to_studio(&mut stream) {
            Ok(ToStudio::Hello { protocol, token }) => {
                if token != self.target.token {
                    return Err("an unexpected program connected to the preview".to_owned());
                }
                if protocol != PROTOCOL_VERSION {
                    return Err(format!(
                        "the game speaks preview protocol {protocol}, Studio speaks \
                         {PROTOCOL_VERSION}. Rebuild the game with this Studio's engine \
                         (File → Upgrade Rust SDK)"
                    ));
                }
            }
            Ok(_) => return Err("the game did not introduce itself".to_owned()),
            Err(err) => return Err(format!("the game's greeting was unreadable: {err}")),
        }
        stream.set_read_timeout(None).map_err(setup)?;
        let reader = stream.try_clone().map_err(setup)?;
        let latest: Latest = Arc::new(Mutex::new(None));
        let (sender, acks) = mpsc::sync_channel(64);
        let slot = Arc::clone(&latest);
        let wake = self.wake.clone();
        std::thread::Builder::new()
            .name("studio-preview-reader".to_owned())
            .spawn(move || read_game(reader, &slot, &sender, wake.as_ref()))
            .map_err(|err| format!("could not read the preview: {err}"))?;
        Ok(Connection {
            writer: stream,
            acks,
            latest,
        })
    }
}

/// Reads the game's messages until the connection ends: frames replace the
/// newest-frame slot, acknowledgements go to the channel.
fn read_game(
    mut reader: TcpStream,
    latest: &Latest,
    acks: &mpsc::SyncSender<SessionEvent>,
    wake: Option<&WakeHandle>,
) {
    loop {
        let event = match protocol::read_to_studio(&mut reader) {
            Ok(ToStudio::Frame(frame)) => {
                match latest.lock() {
                    Ok(mut slot) => *slot = Some(frame),
                    Err(poisoned) => *poisoned.into_inner() = Some(frame),
                }
                None
            }
            Ok(ToStudio::Paused) => Some(SessionEvent::Paused),
            Ok(ToStudio::Resumed) => Some(SessionEvent::Resumed),
            Ok(ToStudio::CursorLock { locked }) => Some(SessionEvent::CursorLock(locked)),
            Ok(ToStudio::Hello { .. }) => None,
            Err(protocol::ProtocolError::Io(_)) => Some(SessionEvent::Closed),
            Err(err) => Some(SessionEvent::Failed(format!(
                "the game sent something Studio cannot read: {err}"
            ))),
        };
        let ended = matches!(event, Some(SessionEvent::Closed | SessionEvent::Failed(_)));
        if let Some(event) = event
            && acks.send(event).is_err()
        {
            break;
        }
        if let Some(wake) = wake {
            let _ = wake.wake();
        }
        if ended {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::embed::protocol::ToStudio;

    fn connect(session: &EmbeddedSession) -> TcpStream {
        TcpStream::connect((Ipv4Addr::LOCALHOST, session.target.port)).expect("connect")
    }

    fn poll_until(
        session: &mut EmbeddedSession,
        done: impl Fn(&[SessionEvent]) -> bool,
    ) -> Vec<SessionEvent> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(session.poll());
            if done(&seen) {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out; saw {seen:?}");
    }

    #[test]
    fn the_right_game_connects_streams_frames_and_closes() {
        let mut session = EmbeddedSession::listen(None).expect("listen");
        let env = EmbedTarget::parse(&session.env_value()).expect("env value");
        let mut game = connect(&session);
        protocol::write_to_studio(
            &mut game,
            &ToStudio::Hello {
                protocol: PROTOCOL_VERSION,
                token: env.token,
            },
        )
        .expect("hello");
        poll_until(&mut session, |seen| seen.contains(&SessionEvent::Connected));
        assert!(session.is_connected());

        session
            .send(&ToGame::Resize {
                width: 64,
                height: 32,
            })
            .expect("send");
        assert_eq!(
            protocol::read_to_game(&mut game).expect("game reads"),
            ToGame::Resize {
                width: 64,
                height: 32
            }
        );

        for sequence in 1..=3 {
            protocol::write_to_studio(
                &mut game,
                &ToStudio::Frame(Frame {
                    sequence,
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                }),
            )
            .expect("frame");
        }
        protocol::write_to_studio(&mut game, &ToStudio::Paused).expect("ack");
        poll_until(&mut session, |seen| seen.contains(&SessionEvent::Paused));
        assert_eq!(
            session.take_frame().map(|frame| frame.sequence),
            Some(3),
            "only the newest frame is kept"
        );
        assert_eq!(session.take_frame(), None);

        drop(game);
        poll_until(&mut session, |seen| seen.contains(&SessionEvent::Closed));
        assert!(!session.is_connected());
    }

    #[test]
    fn the_timeout_counts_from_the_launch_not_the_build() {
        let mut session = EmbeddedSession::listen(None).expect("listen");
        if let Stage::Listening { since, .. } = &session.stage {
            assert!(since.is_none(), "no clock while building");
        }
        assert!(session.poll().is_empty());
        session.game_launched();
        if let Stage::Listening { since, .. } = &mut session.stage {
            *since = Instant::now().checked_sub(HANDSHAKE_TIMEOUT + Duration::from_secs(1));
        }
        let seen = session.poll();
        assert!(
            matches!(&seen[..], [SessionEvent::Failed(message)] if message.contains("preview mode")),
            "{seen:?}"
        );
    }

    #[test]
    fn a_program_without_the_token_is_turned_away() {
        let mut session = EmbeddedSession::listen(None).expect("listen");
        let mut intruder = connect(&session);
        protocol::write_to_studio(
            &mut intruder,
            &ToStudio::Hello {
                protocol: PROTOCOL_VERSION,
                token: "guess".to_owned(),
            },
        )
        .expect("hello");
        let seen = poll_until(&mut session, |seen| {
            seen.iter()
                .any(|event| matches!(event, SessionEvent::Failed(_)))
        });
        assert!(
            matches!(&seen[..], [SessionEvent::Failed(message)] if message.contains("unexpected program"))
        );
        assert!(!session.is_connected());
    }

    #[test]
    fn an_older_protocol_gets_an_upgrade_hint() {
        let mut session = EmbeddedSession::listen(None).expect("listen");
        let token = EmbedTarget::parse(&session.env_value()).expect("env").token;
        let mut game = connect(&session);
        protocol::write_to_studio(
            &mut game,
            &ToStudio::Hello {
                protocol: PROTOCOL_VERSION + 1,
                token,
            },
        )
        .expect("hello");
        let seen = poll_until(&mut session, |seen| !seen.is_empty());
        assert!(
            matches!(&seen[..], [SessionEvent::Failed(message)] if message.contains("Upgrade Rust SDK"))
        );
    }
}
