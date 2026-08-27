//! Sound loading: decodes audio data into a [`StaticSound`] (fully
//! decoded, held in memory — sound effects) or a [`StreamingSound`]
//! (decoded on demand as it plays — music/long tracks). Both decode via
//! kira's own `symphonia`-based decoder (WAV, OGG, MP3, FLAC).

use std::io::Cursor;
use std::path::Path;

use kira::sound::FromFileError;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::sound::streaming::{StreamingSoundData, StreamingSoundHandle};
use kira::track::SpatialTrackHandle;

use crate::error::AudioError;

/// A fully-decoded sound, held entirely in memory.
///
/// Well suited to short sound effects: no per-play disk I/O, safe to play
/// many times (including overlapping/simultaneous plays) with tight
/// timing. For music or other long tracks, prefer [`StreamingSound`] —
/// holding a whole song decoded in memory is wasteful.
pub struct StaticSound {
    pub(crate) data: StaticSoundData,
}

impl StaticSound {
    /// Decodes `bytes` into a [`StaticSound`].
    ///
    /// `looping: true` repeats the whole sound from the start indefinitely
    /// once played, until the returned handle is stopped.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Decode`] if `bytes` isn't a valid/supported
    /// audio format (treat all externally sourced audio bytes as
    /// untrusted — this is the same "don't assume, validate" contract
    /// [`crate::AudioContext::new`] follows for backend initialization).
    pub fn from_bytes(bytes: Vec<u8>, looping: bool) -> Result<Self, AudioError> {
        let data = StaticSoundData::from_cursor(Cursor::new(bytes))
            .map_err(|err| AudioError::Decode(err.to_string()))?;
        Ok(Self {
            data: if looping { data.loop_region(..) } else { data },
        })
    }

    /// Loads and decodes the audio file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Decode`] if the file doesn't exist, can't be
    /// read, or isn't a valid/supported audio format.
    pub fn from_file(path: impl AsRef<Path>, looping: bool) -> Result<Self, AudioError> {
        let data =
            StaticSoundData::from_file(path).map_err(|err| AudioError::Decode(err.to_string()))?;
        Ok(Self {
            data: if looping { data.loop_region(..) } else { data },
        })
    }

    /// Starts playing this sound on `track` — its volume/panning will be
    /// computed relative to whichever listener `track` is bound to (see
    /// [`crate::AudioContext::add_spatial_track`]), instead of playing
    /// flat on the main track like [`crate::AudioContext::play_static`]
    /// does.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Playback`] if playback couldn't start (e.g.
    /// the maximum number of simultaneous sounds was reached).
    pub fn play_on(self, track: &mut SpatialTrackHandle) -> Result<StaticSoundHandle, AudioError> {
        track
            .play(self.data)
            .map_err(|err| AudioError::Playback(err.to_string()))
    }
}

/// A sound decoded from disk on demand as it plays.
///
/// Well suited to music and other long tracks — low memory, since the
/// whole file is never resident at once. For short sound effects, prefer
/// [`StaticSound`] — streaming has per-play disk I/O and startup latency
/// [`StaticSound`] doesn't.
pub struct StreamingSound {
    pub(crate) data: StreamingSoundData<FromFileError>,
}

impl StreamingSound {
    /// Opens the audio file at `path` for streamed playback.
    ///
    /// `looping: true` repeats the whole track from the start indefinitely
    /// once played, until the returned handle is stopped.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Decode`] if the file doesn't exist, can't be
    /// read, or isn't a valid/supported audio format.
    pub fn from_file(path: impl AsRef<Path>, looping: bool) -> Result<Self, AudioError> {
        let data = StreamingSoundData::from_file(path)
            .map_err(|err| AudioError::Decode(err.to_string()))?;
        Ok(Self {
            data: if looping { data.loop_region(..) } else { data },
        })
    }

    /// Starts playing this sound on `track` — see
    /// [`StaticSound::play_on`] for how spatial playback differs from
    /// [`crate::AudioContext::play_streaming`]'s flat main-track playback.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Playback`] if playback couldn't start (e.g.
    /// the maximum number of simultaneous sounds was reached).
    pub fn play_on(
        self,
        track: &mut SpatialTrackHandle,
    ) -> Result<StreamingSoundHandle<FromFileError>, AudioError> {
        track
            .play(self.data)
            .map_err(|err| AudioError::Playback(err.to_string()))
    }
}

/// Encodes a short, real, valid WAV file in memory (16-bit PCM mono,
/// silent) — exercises the actual decode path via kira's real
/// `symphonia` decoder, not a hand-rolled fixture standing in for one.
/// `pub(crate)` (not just `#[cfg(test)]`-local to this module) so
/// `context.rs`'s tests can build a real decodable sound too, without
/// duplicating the RIFF layout.
#[cfg(test)]
pub(crate) fn silent_wav(sample_rate: u32, sample_count: u32) -> Vec<u8> {
    let data_size = sample_count * 2; // 16-bit mono = 2 bytes/sample.
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM.
    wav.extend_from_slice(&1u16.to_le_bytes()); // Mono.
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // Byte rate.
    wav.extend_from_slice(&2u16.to_le_bytes()); // Block align.
    wav.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample.
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend(std::iter::repeat_n(0u8, data_size as usize));
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_decodes_a_valid_wav() {
        let wav = silent_wav(44_100, 1000);
        assert!(StaticSound::from_bytes(wav, false).is_ok());
    }

    // `StaticSound`/`StreamingSound` aren't `Debug` (they hold decoded
    // audio frame data) — `Result::unwrap_err` requires the whole
    // `Result` to be `Debug`, so these match instead, taking care never
    // to format the `Ok` side.
    #[test]
    fn from_bytes_rejects_garbage() {
        match StaticSound::from_bytes(vec![0u8, 1, 2, 3, 4], false) {
            Err(AudioError::Decode(_)) => {}
            Err(other) => panic!("expected AudioError::Decode, got {other:?}"),
            Ok(_) => panic!("expected decoding garbage bytes to fail"),
        }
    }

    #[test]
    fn from_bytes_rejects_empty_input() {
        match StaticSound::from_bytes(Vec::new(), false) {
            Err(AudioError::Decode(_)) => {}
            Err(other) => panic!("expected AudioError::Decode, got {other:?}"),
            Ok(_) => panic!("expected decoding empty bytes to fail"),
        }
    }

    #[test]
    fn from_file_rejects_a_nonexistent_path() {
        match StaticSound::from_file("/nonexistent/path/does-not-exist.wav", false) {
            Err(AudioError::Decode(_)) => {}
            Err(other) => panic!("expected AudioError::Decode, got {other:?}"),
            Ok(_) => panic!("expected loading a nonexistent path to fail"),
        }
    }

    #[test]
    fn streaming_from_file_rejects_a_nonexistent_path() {
        match StreamingSound::from_file("/nonexistent/path/does-not-exist.wav", false) {
            Err(AudioError::Decode(_)) => {}
            Err(other) => panic!("expected AudioError::Decode, got {other:?}"),
            Ok(_) => panic!("expected loading a nonexistent path to fail"),
        }
    }
}
