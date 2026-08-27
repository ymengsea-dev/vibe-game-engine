//! WAV audio import: decode PCM/IEEE-float WAV files to normalized `f32`
//! samples.
//!
//! Hand-rolled rather than pulling in a decoding crate: WAV is a simple,
//! well-specified container (a RIFF chunk list, no compression to speak
//! of for the format tags this supports), and the engine's audio
//! subsystem itself (`engine_audio`, `kira`) doesn't exist yet
//! (Milestone 8) — bringing in a heavier decoder now, for formats nothing
//! can play back yet, isn't worth it. Compressed formats (Vorbis, MP3,
//! ...) can be added if/when `engine_audio` needs them.

use crate::error::AssetError;

/// Decoded WAV audio: interleaved, normalized samples plus enough format
/// info to play them back correctly.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedAudio {
    /// Samples per second, per channel.
    pub sample_rate: u32,
    /// Channel count (`1` = mono, `2` = stereo, ...).
    pub channels: u16,
    /// Interleaved samples (e.g. stereo: `[L0, R0, L1, R1, ...]`),
    /// normalized to `[-1.0, 1.0]` regardless of the source bit depth.
    pub samples: Vec<f32>,
}

/// WAV format tags this importer understands.
const FORMAT_PCM: u16 = 1;
const FORMAT_IEEE_FLOAT: u16 = 3;

/// A cursor over `&[u8]` that returns `None` instead of panicking on
/// out-of-bounds reads — the building block for parsing untrusted WAV
/// bytes without risking a slice-index panic on truncated/malformed
/// input.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(len)?;
        let slice = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }

    fn take_array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn take_u16(&mut self) -> Option<u16> {
        self.take_array::<2>().map(u16::from_le_bytes)
    }

    fn take_u32(&mut self) -> Option<u32> {
        self.take_array::<4>().map(u32::from_le_bytes)
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    /// Skips `len` bytes, e.g. past a chunk's data or its even-alignment
    /// padding byte. No-ops (rather than erroring) if `len` overruns the
    /// buffer — callers that need the skipped bytes read them via
    /// [`Cursor::take`] instead.
    fn skip(&mut self, len: usize) {
        self.position = (self.position + len).min(self.bytes.len());
    }
}

struct FmtChunk {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

/// Decodes a WAV (`.wav`) file already in memory.
///
/// Supports PCM (8/16/24/32-bit integer) and 32-bit IEEE float sample
/// data — the WAV variants that cover the overwhelming majority of real
/// files. `fmt`/`data` chunks may appear in either order; any other
/// chunks (`LIST`, `fact`, ...) are skipped.
///
/// # Errors
///
/// Returns [`AssetError::AudioImport`] if `bytes` isn't a valid WAV file
/// (bad RIFF/WAVE header, truncated chunk, missing `fmt`/`data` chunk),
/// or uses a format tag or bit depth this importer doesn't convert
/// (e.g. ADPCM, 20-bit PCM).
pub fn import_wav_bytes(bytes: &[u8]) -> Result<ImportedAudio, AssetError> {
    let mut cursor = Cursor::new(bytes);

    let riff_tag = cursor
        .take_array::<4>()
        .ok_or_else(|| AssetError::AudioImport("file too short for a RIFF header".into()))?;
    if &riff_tag != b"RIFF" {
        return Err(AssetError::AudioImport(
            "not a RIFF file (missing 'RIFF' tag)".into(),
        ));
    }
    cursor.skip(4); // RIFF chunk size: unused, `data`'s own size is authoritative.

    let wave_tag = cursor
        .take_array::<4>()
        .ok_or_else(|| AssetError::AudioImport("file too short for a WAVE tag".into()))?;
    if &wave_tag != b"WAVE" {
        return Err(AssetError::AudioImport(
            "not a WAVE file (missing 'WAVE' tag)".into(),
        ));
    }

    let mut fmt: Option<FmtChunk> = None;
    let mut data: Option<&[u8]> = None;

    while cursor.remaining() >= 8 {
        let Some(chunk_id) = cursor.take_array::<4>() else {
            break;
        };
        let Some(chunk_size) = cursor.take_u32() else {
            return Err(AssetError::AudioImport(
                "truncated chunk header".to_string(),
            ));
        };
        let chunk_size = chunk_size as usize;

        match &chunk_id {
            b"fmt " => {
                let Some(chunk_bytes) = cursor.take(chunk_size) else {
                    return Err(AssetError::AudioImport(
                        "'fmt ' chunk longer than the file".to_string(),
                    ));
                };
                fmt = Some(parse_fmt_chunk(chunk_bytes)?);
            }
            b"data" => {
                let Some(chunk_bytes) = cursor.take(chunk_size) else {
                    return Err(AssetError::AudioImport(
                        "'data' chunk longer than the file".to_string(),
                    ));
                };
                data = Some(chunk_bytes);
            }
            _ => cursor.skip(chunk_size),
        }
        if !chunk_size.is_multiple_of(2) {
            cursor.skip(1); // RIFF chunks are padded to an even length.
        }
    }

    let fmt = fmt.ok_or_else(|| AssetError::AudioImport("missing 'fmt ' chunk".to_string()))?;
    let data = data.ok_or_else(|| AssetError::AudioImport("missing 'data' chunk".to_string()))?;

    let samples = decode_samples(&fmt, data)?;
    Ok(ImportedAudio {
        sample_rate: fmt.sample_rate,
        channels: fmt.channels,
        samples,
    })
}

fn parse_fmt_chunk(bytes: &[u8]) -> Result<FmtChunk, AssetError> {
    let mut cursor = Cursor::new(bytes);
    let audio_format = cursor
        .take_u16()
        .ok_or_else(|| AssetError::AudioImport("'fmt ' chunk too short".to_string()))?;
    let channels = cursor
        .take_u16()
        .ok_or_else(|| AssetError::AudioImport("'fmt ' chunk too short".to_string()))?;
    let sample_rate = cursor
        .take_u32()
        .ok_or_else(|| AssetError::AudioImport("'fmt ' chunk too short".to_string()))?;
    cursor.skip(4); // byte rate: derivable, unused.
    cursor.skip(2); // block align: derivable, unused.
    let bits_per_sample = cursor
        .take_u16()
        .ok_or_else(|| AssetError::AudioImport("'fmt ' chunk too short".to_string()))?;

    if channels == 0 {
        return Err(AssetError::AudioImport(
            "'fmt ' chunk declares 0 channels".to_string(),
        ));
    }

    Ok(FmtChunk {
        audio_format,
        channels,
        sample_rate,
        bits_per_sample,
    })
}

fn decode_samples(fmt: &FmtChunk, data: &[u8]) -> Result<Vec<f32>, AssetError> {
    match (fmt.audio_format, fmt.bits_per_sample) {
        (FORMAT_PCM, 8) => Ok(data
            .iter()
            .map(|&b| (f32::from(b) - 128.0) / 128.0)
            .collect()),
        (FORMAT_PCM, 16) => data
            .chunks_exact(2)
            .map(|chunk| {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                Ok(f32::from(sample) / f32::from(i16::MAX))
            })
            .collect(),
        (FORMAT_PCM, 24) => data
            .chunks_exact(3)
            .map(|chunk| {
                let mut padded = [0u8; 4];
                padded[1..4].copy_from_slice(chunk);
                // Sign-extend: shift a 24-bit value into the top 24 bits
                // of an i32, then arithmetic-shift back down by 8.
                let sample = i32::from_le_bytes(padded) >> 8;
                Ok(sample as f32 / 8_388_608.0)
            })
            .collect(),
        (FORMAT_PCM, 32) => data
            .chunks_exact(4)
            .map(|chunk| {
                let sample = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                Ok(sample as f32 / i32::MAX as f32)
            })
            .collect(),
        (FORMAT_IEEE_FLOAT, 32) => data
            .chunks_exact(4)
            .map(|chunk| Ok(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])))
            .collect(),
        (format, bits) => Err(AssetError::AudioImport(format!(
            "unsupported WAV format (tag {format}, {bits}-bit) — only PCM 8/16/24/32-bit and 32-bit float are converted"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_wav(
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        audio_format: u16,
        data: &[u8],
    ) -> Vec<u8> {
        let byte_rate = sample_rate * u32::from(channels) * (u32::from(bits_per_sample) / 8);
        let block_align = channels * (bits_per_sample / 8);
        let fmt_chunk_size: u32 = 16;
        let data_size = data.len() as u32;
        let riff_size = 4 + (8 + fmt_chunk_size) + (8 + data_size);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_size.to_le_bytes());
        out.extend_from_slice(b"WAVE");

        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&fmt_chunk_size.to_le_bytes());
        out.extend_from_slice(&audio_format.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits_per_sample.to_le_bytes());

        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_size.to_le_bytes());
        out.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            out.push(0);
        }

        out
    }

    #[test]
    fn imports_16_bit_mono_pcm() {
        let mut data = Vec::new();
        for sample in [0i16, i16::MAX, i16::MIN, -1000] {
            data.extend_from_slice(&sample.to_le_bytes());
        }
        let wav = build_wav(44_100, 1, 16, FORMAT_PCM, &data);

        let imported = import_wav_bytes(&wav).unwrap();
        assert_eq!(imported.sample_rate, 44_100);
        assert_eq!(imported.channels, 1);
        assert_eq!(imported.samples.len(), 4);
        assert!((imported.samples[0] - 0.0).abs() < 1e-6);
        assert!((imported.samples[1] - 1.0).abs() < 1e-6);
        assert!((imported.samples[2] - (-1.0)).abs() < 1e-4);
    }

    #[test]
    fn imports_8_bit_pcm() {
        let data = vec![0u8, 128, 255];
        let wav = build_wav(8_000, 1, 8, FORMAT_PCM, &data);

        let imported = import_wav_bytes(&wav).unwrap();
        assert!((imported.samples[0] - (-1.0)).abs() < 1e-6);
        assert!((imported.samples[1] - 0.0).abs() < 1e-6);
        assert!((imported.samples[2] - 1.0).abs() < 1e-2);
    }

    #[test]
    fn imports_32_bit_float_unchanged() {
        let mut data = Vec::new();
        for sample in [0.25f32, -0.5, 1.0] {
            data.extend_from_slice(&sample.to_le_bytes());
        }
        let wav = build_wav(48_000, 1, 32, FORMAT_IEEE_FLOAT, &data);

        let imported = import_wav_bytes(&wav).unwrap();
        assert_eq!(imported.samples, vec![0.25, -0.5, 1.0]);
    }

    #[test]
    fn preserves_stereo_interleaving() {
        let mut data = Vec::new();
        for sample in [1i16, -1, 2, -2] {
            data.extend_from_slice(&sample.to_le_bytes());
        }
        let wav = build_wav(44_100, 2, 16, FORMAT_PCM, &data);

        let imported = import_wav_bytes(&wav).unwrap();
        assert_eq!(imported.channels, 2);
        assert_eq!(imported.samples.len(), 4);
    }

    #[test]
    fn rejects_non_riff_bytes() {
        let err = import_wav_bytes(b"not a wav file at all").unwrap_err();
        assert!(matches!(err, AssetError::AudioImport(_)));
    }

    #[test]
    fn rejects_empty_input() {
        let err = import_wav_bytes(&[]).unwrap_err();
        assert!(matches!(err, AssetError::AudioImport(_)));
    }

    #[test]
    fn rejects_truncated_data_chunk() {
        let mut wav = build_wav(44_100, 1, 16, FORMAT_PCM, &[0, 0, 0, 0]);
        wav.truncate(wav.len() - 2); // chop off the last declared sample's bytes
        let err = import_wav_bytes(&wav).unwrap_err();
        assert!(matches!(err, AssetError::AudioImport(_)));
    }

    #[test]
    fn rejects_unsupported_format_tag() {
        let wav = build_wav(44_100, 1, 4, 2, &[0, 0]); // format tag 2 = ADPCM
        let err = import_wav_bytes(&wav).unwrap_err();
        assert!(matches!(err, AssetError::AudioImport(_)));
    }

    #[test]
    fn rejects_zero_channels() {
        let wav = build_wav(44_100, 0, 16, FORMAT_PCM, &[0, 0]);
        let err = import_wav_bytes(&wav).unwrap_err();
        assert!(matches!(err, AssetError::AudioImport(_)));
    }
}
