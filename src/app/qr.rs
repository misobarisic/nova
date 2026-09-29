//! QR encoding of the sync invite ticket.
//!
//! The invite ticket (`NV1` + base32) is the only payload the QR carries —
//! scanning it is equivalent to pasting it, so no protocol change is needed.
//! The matrix is rendered to an RGB buffer and handed to Slint as an `Image`;
//! `settings.slint` draws it with `image-rendering: pixelated` so the module
//! edges stay sharp when the image is scaled to the layout size.

use qrcode::QrCode;
use qrcode::types::Color;
use slint::{Image, Rgb8Pixel, SharedPixelBuffer};

/// Modules of white around the symbol. QR readers need this quiet zone to
/// locate the finder patterns; 4 is the spec minimum.
const QUIET_ZONE: usize = 4;
/// Device pixels per module in the generated raster. Large enough that the
/// image stays crisp after Slint scales it down; `pixelated` does the rest.
const MODULE_SCALE: usize = 6;

/// Build the invite QR as a Slint image, or `None` if the ticket does not fit
/// a QR symbol (it always should — the ticket is ~82 alphanumeric chars).
pub(crate) fn invite_qr_image(ticket: &str) -> Option<Image> {
    let (side, modules) = qr_modules(ticket)?;
    let scale = MODULE_SCALE;
    let pixels = side * scale;
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(pixels as u32, pixels as u32);
    let bytes = buffer.make_mut_bytes();
    for y in 0..side {
        for x in 0..side {
            let value = if modules[y * side + x] { 0u8 } else { 255u8 };
            // Stamp one `scale × scale` block per module (row-major RGB).
            for dy in 0..scale {
                let row = (y * scale + dy) * pixels;
                for dx in 0..scale {
                    let offset = (row + x * scale + dx) * 3;
                    bytes[offset..offset + 3].fill(value);
                }
            }
        }
    }
    Some(Image::from_rgb8(buffer))
}

/// Module grid (`side × side`, row-major) for `ticket`, including the quiet
/// zone. `true` = dark module. Kept separate from the raster so the round-trip
/// test can decode it without going through Slint's image type.
fn qr_modules(ticket: &str) -> Option<(usize, Vec<bool>)> {
    let code = QrCode::new(ticket.as_bytes()).ok()?;
    let colors = code.to_colors();
    let modules = code.width();
    let side = modules + QUIET_ZONE * 2;
    let mut grid = vec![false; side * side];
    for y in 0..modules {
        for x in 0..modules {
            grid[(y + QUIET_ZONE) * side + (x + QUIET_ZONE)] =
                colors[y * modules + x] == Color::Dark;
        }
    }
    Some((side, grid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display and scan halves must agree: a ticket encoded to modules and
    /// fed back through the decoder (the same crate the Android scanner uses)
    /// yields the ticket. Guards the payload path without a camera or device.
    #[test]
    fn qr_round_trips_the_ticket() {
        let id = iroh::SecretKey::generate().public();
        let secret = [42u8; 16];
        let ticket = nova_sync::encode_ticket(&id, &secret);

        let (side, modules) = qr_modules(&ticket).expect("ticket fits a QR symbol");
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| {
            if modules[y * side + x] { 0 } else { 255 }
        });
        let grids = image.detect_grids();
        let decoded = grids
            .iter()
            .find_map(|grid| grid.decode().ok())
            .expect("QR decodes");
        assert_eq!(decoded.1, ticket);
        assert!(nova_sync::decode_ticket(&decoded.1).is_ok());
    }

    /// The raster builder runs without panicking and produces an image for a
    /// real ticket (guards the block-fill indexing/overflow arithmetic).
    #[test]
    fn qr_image_builds() {
        let id = iroh::SecretKey::generate().public();
        let secret = [1u8; 16];
        let ticket = nova_sync::encode_ticket(&id, &secret);
        assert!(invite_qr_image(&ticket).is_some());
    }
}
