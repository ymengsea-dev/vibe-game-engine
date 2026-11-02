//! The messages Studio and an embedded game exchange, and their wire format.
//!
//! One TCP connection on `127.0.0.1` carries both directions. Every message
//! is a 5-byte header, `[kind: u8][length: u32 LE]`, followed by `length`
//! payload bytes:
//!
//! | Kind | Payload |
//! | --- | --- |
//! | [`KIND_CONTROL`] | A JSON [`ToGame`] or [`ToStudio`] (without frames), at most [`MAX_CONTROL_BYTES`] |
//! | [`KIND_FRAME`] | `[sequence: u64][width: u32][height: u32]` then `width × height × 4` RGBA8 bytes |
//!
//! Frames are raw bytes because they are large (3.7 MB at 720p) and
//! frequent; everything else is JSON so it stays readable and easy to
//! extend. Readers reject an unknown kind, an oversized payload, or a frame
//! whose size does not match its header, so a confused or hostile peer
//! cannot make the other side allocate without limit.

use std::io::{Read, Write};

use engine_platform::{KeyCode, MouseButton, PlatformEvent};
use serde::{Deserialize, Serialize};

/// Bumped whenever a message changes incompatibly; [`ToStudio::Hello`]
/// carries it so a game built against an older engine is refused with a
/// readable message instead of misbehaving.
pub const PROTOCOL_VERSION: u32 = 1;

/// Header kind of a JSON control message.
pub const KIND_CONTROL: u8 = 1;
/// Header kind of a frame.
pub const KIND_FRAME: u8 = 2;

/// Largest JSON control message, in bytes.
pub const MAX_CONTROL_BYTES: u32 = 64 * 1024;

/// Largest frame side, in pixels.
pub const MAX_FRAME_SIDE: u32 = 4096;

/// Bytes in a frame payload before its pixels.
const FRAME_HEADER_BYTES: usize = 16;

/// Keyboard and mouse input Studio forwards to the game: the subset of
/// [`PlatformEvent`] a player produces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// A key changed state.
    Key {
        /// Which physical key.
        key: KeyCode,
        /// `true` if pressed.
        pressed: bool,
        /// `true` for an OS auto-repeat.
        repeat: bool,
    },
    /// A mouse button changed state.
    MouseButton {
        /// Which button.
        button: MouseButton,
        /// `true` if pressed.
        pressed: bool,
    },
    /// The cursor moved, in frame pixels from the frame's top-left corner.
    CursorMoved {
        /// X position.
        x: f64,
        /// Y position.
        y: f64,
    },
    /// The wheel or trackpad scrolled.
    Wheel {
        /// Horizontal delta.
        delta_x: f32,
        /// Vertical delta.
        delta_y: f32,
    },
    /// The game view gained or lost keyboard focus.
    Focus {
        /// `true` when focused.
        focused: bool,
    },
}

impl InputEvent {
    /// The input part of `event`, or `None` for events that are not player
    /// input (resize, redraw, close, file drops).
    pub fn from_platform(event: &PlatformEvent) -> Option<Self> {
        Some(match *event {
            PlatformEvent::KeyboardInput {
                key,
                pressed,
                repeat,
            } => Self::Key {
                key,
                pressed,
                repeat,
            },
            PlatformEvent::MouseButtonInput { button, pressed } => {
                Self::MouseButton { button, pressed }
            }
            PlatformEvent::CursorMoved { x, y } => Self::CursorMoved { x, y },
            PlatformEvent::MouseWheel { delta_x, delta_y } => Self::Wheel { delta_x, delta_y },
            PlatformEvent::FocusChanged { focused } => Self::Focus { focused },
            PlatformEvent::Resized { .. }
            | PlatformEvent::RedrawRequested
            | PlatformEvent::CloseRequested
            | PlatformEvent::FileDropped { .. } => return None,
        })
    }

    /// The [`PlatformEvent`] a window would have delivered for this input.
    pub fn into_platform(self) -> PlatformEvent {
        match self {
            Self::Key {
                key,
                pressed,
                repeat,
            } => PlatformEvent::KeyboardInput {
                key,
                pressed,
                repeat,
            },
            Self::MouseButton { button, pressed } => {
                PlatformEvent::MouseButtonInput { button, pressed }
            }
            Self::CursorMoved { x, y } => PlatformEvent::CursorMoved { x, y },
            Self::Wheel { delta_x, delta_y } => PlatformEvent::MouseWheel { delta_x, delta_y },
            Self::Focus { focused } => PlatformEvent::FocusChanged { focused },
        }
    }
}

/// Studio → game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToGame {
    /// Render at this size, in physical pixels (both nonzero).
    Resize {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// Player input.
    Input(InputEvent),
    /// Freeze the game: no time passes, nothing updates, audio is muted.
    Pause,
    /// Continue after [`ToGame::Pause`].
    Resume,
    /// While paused, run exactly one frame.
    Step,
    /// Quit as if the window were closed (saves flush).
    Stop,
}

/// Game → Studio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToStudio {
    /// The first message: proves the game is the one Studio launched.
    Hello {
        /// The game's [`PROTOCOL_VERSION`].
        protocol: u32,
        /// The token Studio passed in the game's environment.
        token: String,
    },
    /// The game acknowledged [`ToGame::Pause`].
    Paused,
    /// The game acknowledged [`ToGame::Resume`].
    Resumed,
    /// A rendered frame. Sent as [`KIND_FRAME`], never as JSON.
    #[serde(skip)]
    Frame(Frame),
}

/// One rendered frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Increases by one per frame the game sent.
    pub sequence: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed sRGB RGBA8 pixels, `width × height × 4` bytes.
    pub rgba: Vec<u8>,
}

/// Why a message could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The connection failed or closed.
    #[error("connection error: {0}")]
    Io(#[from] std::io::Error),
    /// The header named a kind this side does not understand.
    #[error("unknown message kind {0}")]
    UnknownKind(u8),
    /// A payload exceeded its limit.
    #[error("message of {length} bytes exceeds the {limit}-byte limit")]
    TooLarge {
        /// Announced length.
        length: u64,
        /// Allowed length.
        limit: u64,
    },
    /// A control message was not valid JSON for its direction.
    #[error("malformed control message: {0}")]
    Malformed(String),
    /// A frame's header and length disagree, or its size is out of range.
    #[error("malformed frame: {0}")]
    BadFrame(String),
}

fn write_message(writer: &mut impl Write, kind: u8, parts: &[&[u8]]) -> std::io::Result<()> {
    let length: usize = parts.iter().map(|part| part.len()).sum();
    let length = u32::try_from(length).map_err(std::io::Error::other)?;
    let mut header = [0u8; 5];
    header[0] = kind;
    header[1..].copy_from_slice(&length.to_le_bytes());
    writer.write_all(&header)?;
    for part in parts {
        writer.write_all(part)?;
    }
    writer.flush()
}

fn read_header(reader: &mut impl Read) -> Result<(u8, u32), ProtocolError> {
    let mut header = [0u8; 5];
    reader.read_exact(&mut header)?;
    let mut length = [0u8; 4];
    length.copy_from_slice(&header[1..]);
    Ok((header[0], u32::from_le_bytes(length)))
}

fn read_control<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
    length: u32,
) -> Result<T, ProtocolError> {
    if length > MAX_CONTROL_BYTES {
        return Err(ProtocolError::TooLarge {
            length: u64::from(length),
            limit: u64::from(MAX_CONTROL_BYTES),
        });
    }
    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|err| ProtocolError::Malformed(err.to_string()))
}

/// Writes one Studio → game message.
///
/// # Errors
///
/// Any I/O error from `writer`.
pub fn write_to_game(writer: &mut impl Write, message: &ToGame) -> std::io::Result<()> {
    let json = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    write_message(writer, KIND_CONTROL, &[&json])
}

/// Reads one Studio → game message.
///
/// # Errors
///
/// [`ProtocolError`] for I/O failures (including a closed connection), an
/// unknown kind or a frame (Studio never sends one), an oversized or
/// malformed payload.
pub fn read_to_game(reader: &mut impl Read) -> Result<ToGame, ProtocolError> {
    match read_header(reader)? {
        (KIND_CONTROL, length) => read_control(reader, length),
        (kind, _) => Err(ProtocolError::UnknownKind(kind)),
    }
}

/// Writes one game → Studio message; [`ToStudio::Frame`] goes as raw bytes.
///
/// # Errors
///
/// Any I/O error from `writer`.
pub fn write_to_studio(writer: &mut impl Write, message: &ToStudio) -> std::io::Result<()> {
    match message {
        ToStudio::Frame(frame) => {
            let mut header = [0u8; FRAME_HEADER_BYTES];
            header[..8].copy_from_slice(&frame.sequence.to_le_bytes());
            header[8..12].copy_from_slice(&frame.width.to_le_bytes());
            header[12..].copy_from_slice(&frame.height.to_le_bytes());
            write_message(writer, KIND_FRAME, &[&header, &frame.rgba])
        }
        control => {
            let json = serde_json::to_vec(control).map_err(std::io::Error::other)?;
            write_message(writer, KIND_CONTROL, &[&json])
        }
    }
}

/// Reads one game → Studio message.
///
/// # Errors
///
/// [`ProtocolError`] for I/O failures (including a closed connection), an
/// unknown kind, an oversized or malformed payload, or a frame whose size
/// does not match its header or exceeds [`MAX_FRAME_SIDE`].
pub fn read_to_studio(reader: &mut impl Read) -> Result<ToStudio, ProtocolError> {
    match read_header(reader)? {
        (KIND_CONTROL, length) => read_control(reader, length),
        (KIND_FRAME, length) => read_frame(reader, length).map(ToStudio::Frame),
        (kind, _) => Err(ProtocolError::UnknownKind(kind)),
    }
}

fn read_frame(reader: &mut impl Read, length: u32) -> Result<Frame, ProtocolError> {
    let limit = FRAME_HEADER_BYTES as u64 + u64::from(MAX_FRAME_SIDE).pow(2) * 4;
    if u64::from(length) > limit {
        return Err(ProtocolError::TooLarge {
            length: u64::from(length),
            limit,
        });
    }
    let length = length as usize;
    if length < FRAME_HEADER_BYTES {
        return Err(ProtocolError::BadFrame(format!(
            "{length} bytes is shorter than a frame header"
        )));
    }
    let mut header = [0u8; FRAME_HEADER_BYTES];
    reader.read_exact(&mut header)?;
    let (sequence, size) = header.split_at(8);
    let (width, height) = size.split_at(4);
    let mut word = [0u8; 4];
    word.copy_from_slice(width);
    let width = u32::from_le_bytes(word);
    word.copy_from_slice(height);
    let height = u32::from_le_bytes(word);
    let mut long = [0u8; 8];
    long.copy_from_slice(sequence);
    let sequence = u64::from_le_bytes(long);
    if width == 0 || height == 0 || width > MAX_FRAME_SIDE || height > MAX_FRAME_SIDE {
        return Err(ProtocolError::BadFrame(format!(
            "{width}x{height} is outside 1..={MAX_FRAME_SIDE}"
        )));
    }
    let pixels = width as usize * height as usize * 4;
    if pixels != length - FRAME_HEADER_BYTES {
        return Err(ProtocolError::BadFrame(format!(
            "{width}x{height} needs {pixels} bytes, got {}",
            length - FRAME_HEADER_BYTES
        )));
    }
    let mut rgba = vec![0u8; pixels];
    reader.read_exact(&mut rgba)?;
    Ok(Frame {
        sequence,
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip_to_game(message: ToGame) -> ToGame {
        let mut bytes = Vec::new();
        write_to_game(&mut bytes, &message).expect("write");
        read_to_game(&mut Cursor::new(bytes)).expect("read")
    }

    fn round_trip_to_studio(message: ToStudio) -> ToStudio {
        let mut bytes = Vec::new();
        write_to_studio(&mut bytes, &message).expect("write");
        read_to_studio(&mut Cursor::new(bytes)).expect("read")
    }

    #[test]
    fn every_studio_message_survives_the_wire() {
        for message in [
            ToGame::Resize {
                width: 1280,
                height: 720,
            },
            ToGame::Input(InputEvent::Key {
                key: KeyCode::KeyW,
                pressed: true,
                repeat: false,
            }),
            ToGame::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }),
            ToGame::Input(InputEvent::CursorMoved { x: 10.5, y: 20.0 }),
            ToGame::Input(InputEvent::Wheel {
                delta_x: 0.0,
                delta_y: -1.0,
            }),
            ToGame::Input(InputEvent::Focus { focused: true }),
            ToGame::Pause,
            ToGame::Resume,
            ToGame::Step,
            ToGame::Stop,
        ] {
            assert_eq!(round_trip_to_game(message.clone()), message);
        }
    }

    #[test]
    fn every_game_message_survives_the_wire() {
        for message in [
            ToStudio::Hello {
                protocol: PROTOCOL_VERSION,
                token: "abc".to_owned(),
            },
            ToStudio::Paused,
            ToStudio::Resumed,
            ToStudio::Frame(Frame {
                sequence: 7,
                width: 2,
                height: 1,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            }),
        ] {
            assert_eq!(round_trip_to_studio(message.clone()), message);
        }
    }

    #[test]
    fn several_messages_read_back_in_order_from_one_stream() {
        let mut bytes = Vec::new();
        write_to_game(&mut bytes, &ToGame::Pause).expect("write");
        write_to_game(&mut bytes, &ToGame::Step).expect("write");
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_to_game(&mut reader).expect("first"), ToGame::Pause);
        assert_eq!(read_to_game(&mut reader).expect("second"), ToGame::Step);
        assert!(matches!(
            read_to_game(&mut reader),
            Err(ProtocolError::Io(_))
        ));
    }

    fn header(kind: u8, length: u32) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes
    }

    #[test]
    fn hostile_or_broken_messages_are_refused_before_allocating() {
        let unknown = header(9, 0);
        assert!(matches!(
            read_to_studio(&mut Cursor::new(unknown)),
            Err(ProtocolError::UnknownKind(9))
        ));
        let huge_control = header(KIND_CONTROL, MAX_CONTROL_BYTES + 1);
        assert!(matches!(
            read_to_game(&mut Cursor::new(huge_control)),
            Err(ProtocolError::TooLarge { .. })
        ));
        let huge_frame = header(KIND_FRAME, u32::MAX);
        assert!(matches!(
            read_to_studio(&mut Cursor::new(huge_frame)),
            Err(ProtocolError::TooLarge { .. })
        ));
        assert!(
            matches!(
                read_to_game(&mut Cursor::new(header(KIND_FRAME, 16))),
                Err(ProtocolError::UnknownKind(KIND_FRAME))
            ),
            "Studio never sends frames"
        );

        let mut garbage = header(KIND_CONTROL, 5);
        garbage.extend_from_slice(b"nope!");
        assert!(matches!(
            read_to_game(&mut Cursor::new(garbage)),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn a_frame_must_match_its_announced_size() {
        let frame = |width: u32, height: u32, pixels: usize| {
            let mut bytes = header(KIND_FRAME, (FRAME_HEADER_BYTES + pixels) as u32);
            bytes.extend_from_slice(&0u64.to_le_bytes());
            bytes.extend_from_slice(&width.to_le_bytes());
            bytes.extend_from_slice(&height.to_le_bytes());
            bytes.extend(std::iter::repeat_n(0u8, pixels));
            bytes
        };
        assert!(read_to_studio(&mut Cursor::new(frame(2, 2, 16))).is_ok());
        for bad in [
            frame(2, 2, 12),
            frame(0, 2, 0),
            frame(MAX_FRAME_SIDE + 1, 1, 0),
        ] {
            assert!(matches!(
                read_to_studio(&mut Cursor::new(bad)),
                Err(ProtocolError::BadFrame(_))
            ));
        }
    }

    #[test]
    fn only_player_input_converts_from_platform_events() {
        let key = PlatformEvent::KeyboardInput {
            key: KeyCode::Space,
            pressed: true,
            repeat: false,
        };
        let input = InputEvent::from_platform(&key).expect("input");
        assert_eq!(input.into_platform(), key);
        assert_eq!(
            InputEvent::from_platform(&PlatformEvent::RedrawRequested),
            None
        );
        assert_eq!(
            InputEvent::from_platform(&PlatformEvent::Resized {
                width: 1,
                height: 1
            }),
            None
        );
    }
}
