//! Game icons for exports: the macOS `.icns` file and the PNG the game window
//! shows on Windows and Linux, both made from one project image.
//!
//! The `.icns` container is written here (PNG-compressed entries, the form
//! macOS has read since 10.7), so exporting needs no Apple tools.

use image::{RgbaImage, imageops::FilterType};

/// Size of the PNG a game's window uses as its icon.
pub const WINDOW_ICON_SIZE: u32 = 256;

/// The `.icns` entries written: (type code, pixel size).
const ICNS_ENTRIES: [(&[u8; 4], u32); 6] = [
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// Why an icon couldn't be made.
#[derive(Debug, thiserror::Error)]
pub enum IconError {
    /// The bytes aren't an image this build can read.
    #[error("the icon is not a readable image: {0}")]
    Decode(String),
    /// Writing a PNG failed.
    #[error("could not write the icon: {0}")]
    Encode(String),
}

/// Decodes `bytes` and centres the image on a transparent square, so a
/// wide or tall picture isn't stretched.
///
/// # Errors
///
/// [`IconError::Decode`] when the bytes aren't an image.
pub fn square_icon(bytes: &[u8]) -> Result<RgbaImage, IconError> {
    let image = image::load_from_memory(bytes)
        .map_err(|err| IconError::Decode(err.to_string()))?
        .to_rgba8();
    let side = image.width().max(image.height()).max(1);
    let mut square = RgbaImage::new(side, side);
    let x = i64::from((side - image.width()) / 2);
    let y = i64::from((side - image.height()) / 2);
    image::imageops::overlay(&mut square, &image, x, y);
    Ok(square)
}

/// `icon` scaled to `size` × `size` and encoded as PNG.
///
/// # Errors
///
/// [`IconError::Encode`] when encoding fails.
pub fn png_at(icon: &RgbaImage, size: u32) -> Result<Vec<u8>, IconError> {
    let scaled = image::imageops::resize(icon, size, size, FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    scaled
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|err| IconError::Encode(err.to_string()))?;
    Ok(out.into_inner())
}

/// A macOS `.icns` file holding `icon` at every size Finder and the Dock use.
///
/// # Errors
///
/// [`IconError::Encode`] when a size can't be encoded.
pub fn icns(icon: &RgbaImage) -> Result<Vec<u8>, IconError> {
    let mut body = Vec::new();
    for (code, size) in ICNS_ENTRIES {
        let png = png_at(icon, size)?;
        let length = u32::try_from(png.len() + 8)
            .map_err(|_| IconError::Encode("icon too large".to_owned()))?;
        body.extend_from_slice(code);
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&png);
    }
    let total = u32::try_from(body.len() + 8)
        .map_err(|_| IconError::Encode("icon too large".to_owned()))?;
    let mut file = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"icns");
    file.extend_from_slice(&total.to_be_bytes());
    file.extend_from_slice(&body);
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([200, 40, 40, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_wide_image_is_centred_on_a_clear_square() {
        let square = square_icon(&png(40, 20)).unwrap();
        assert_eq!(square.dimensions(), (40, 40));
        assert_eq!(square.get_pixel(0, 0)[3], 0, "padding is clear");
        assert_eq!(square.get_pixel(20, 20)[3], 255);
        assert!(square_icon(b"not an image").is_err());
    }

    #[test]
    fn the_icns_file_lists_every_size_as_png() {
        let icon = square_icon(&png(16, 16)).unwrap();
        let file = icns(&icon).unwrap();
        assert_eq!(&file[..4], b"icns");
        let total = u32::from_be_bytes([file[4], file[5], file[6], file[7]]) as usize;
        assert_eq!(total, file.len());
        let mut at = 8;
        let mut sizes = Vec::new();
        while at < file.len() {
            let code = &file[at..at + 4];
            let length =
                u32::from_be_bytes([file[at + 4], file[at + 5], file[at + 6], file[at + 7]])
                    as usize;
            let entry = &file[at + 8..at + length];
            assert_eq!(&entry[..8], b"\x89PNG\r\n\x1a\n", "{code:?} holds a PNG");
            let image = image::load_from_memory(entry).unwrap();
            sizes.push((code.to_vec(), image.width()));
            at += length;
        }
        assert_eq!(at, file.len());
        let expected: Vec<(Vec<u8>, u32)> = ICNS_ENTRIES
            .iter()
            .map(|(code, size)| (code.to_vec(), *size))
            .collect();
        assert_eq!(sizes, expected);
    }
}
