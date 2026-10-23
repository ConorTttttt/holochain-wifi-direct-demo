//! QR codes: drawing an invitation, and reading one back from a camera, an
//! image file, or pasted text.

use anyhow::{Context, Result, bail};
use qrcode::render::unicode::Dense1x2;
use qrcode::{EcLevel, QrCode};
use std::path::{Path, PathBuf};

fn code(text: &str) -> Result<QrCode> {
    // Level M survives a slightly blurred phone photo of a screen.
    QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .context("the invitation does not fit in a QR code")
}

/// The QR code as text, for a terminal with a dark background.
pub fn render_terminal(text: &str) -> Result<String> {
    Ok(code(text)?
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

/// Write the QR code as a PNG that only this user can read, and delete it
/// when dropped: it holds the live Wi-Fi passphrase and pairing secret.
pub struct QrImage {
    path: PathBuf,
}

impl QrImage {
    pub fn write(text: &str, path: &Path) -> Result<Self> {
        let image = code(text)?
            .render::<image::Luma<u8>>()
            .quiet_zone(true)
            .min_dimensions(600, 600)
            .build();
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .context("encoding the QR code")?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        use std::io::Write;
        options
            .open(path)
            .and_then(|mut file| file.write_all(&png))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Show it in the desktop's image viewer.
    pub fn open(&self) -> Result<()> {
        let path = self.path.as_os_str();
        let status = if cfg!(windows) {
            // `start` needs a window title argument before the path.
            std::process::Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(path)
                .status()
        } else if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(path).status()
        } else {
            std::process::Command::new("xdg-open").arg(path).status()
        };
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => bail!("the image viewer exited with {status}"),
            Err(err) => bail!("could not start an image viewer: {err}"),
        }
    }
}

impl Drop for QrImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read a QR code from an image file: a PNG saved by `hc-wfd create`, or a
/// phone photo of the Windows screen.
pub fn decode_image(path: &Path) -> Result<String> {
    let image = image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_luma8();
    if let Some(text) = decode_luma(image.clone()) {
        return Ok(text);
    }
    // Large photos decode better, and faster, scaled down.
    let (w, h) = image.dimensions();
    for target in [1600u32, 1000, 700] {
        if w.max(h) > target {
            let scale = f64::from(target) / f64::from(w.max(h));
            let small = image::imageops::resize(
                &image,
                (f64::from(w) * scale) as u32,
                (f64::from(h) * scale) as u32,
                image::imageops::FilterType::Triangle,
            );
            if let Some(text) = decode_luma(small) {
                return Ok(text);
            }
        }
    }
    bail!(
        "no readable QR code in {}. Use the PNG that `hc-wfd create` saved, or a sharp, \
         straight photo of the whole code",
        path.display()
    )
}

fn decode_luma(image: image::GrayImage) -> Option<String> {
    let mut prepared = rqrr::PreparedImage::prepare(image);
    prepared
        .detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok().map(|(_meta, content)| content))
}

/// Read a QR code with a webcam, through `zbarcam` (Ubuntu: `sudo apt install
/// zbar-tools`). Returns the first code seen.
pub fn scan_camera(device: Option<&str>) -> Result<String> {
    let mut command = std::process::Command::new("zbarcam");
    command.args([
        "--raw",
        "--oneshot",
        "-q",
        "--set",
        "*.enable=0",
        "--set",
        "qrcode.enable=1",
    ]);
    if let Some(device) = device {
        command.arg(device);
    }
    println!("camera    hold the QR code up to the webcam; close the preview window to cancel");
    let output = match command.output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => bail!(
            "webcam scanning uses zbarcam, which is not installed (sudo apt install \
             zbar-tools). Or import the QR image, or paste the invitation text"
        ),
        Err(err) => bail!("could not run zbarcam: {err}"),
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        bail!(
            "zbarcam read no QR code ({}). Is there a webcam? {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hcwfd_pairing::Invite;
    use hcwfd_pairing::invite::DEFAULT_LIFETIME_S;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hc-wfd-qr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn an_invitation_survives_png_and_back() {
        let invite = Invite::generate(47002, DEFAULT_LIFETIME_S, 1_761_300_000);
        let text = invite.encode();
        let path = scratch("invite.png");
        let png = QrImage::write(&text, &path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "the QR image must be private: {mode:o}");
        }
        assert_eq!(decode_image(png.path()).unwrap(), text);
        drop(png);
        assert!(
            !path.exists(),
            "the QR image must be deleted with its guard"
        );
    }

    #[test]
    fn a_scaled_noisy_photo_still_decodes() {
        let text = Invite::generate(47002, DEFAULT_LIFETIME_S, 1_761_300_000).encode();
        let qr = code(&text)
            .unwrap()
            .render::<image::Luma<u8>>()
            .quiet_zone(true)
            .min_dimensions(500, 500)
            .build();
        // A large grey "photo" with the code off-centre and some noise.
        let mut photo = image::GrayImage::from_pixel(3000, 2200, image::Luma([170]));
        image::imageops::overlay(&mut photo, &qr, 900, 600);
        for (x, y, pixel) in photo.enumerate_pixels_mut() {
            if (x * 7 + y * 13) % 29 == 0 {
                pixel.0[0] = pixel.0[0].saturating_add(40);
            }
        }
        let path = scratch("photo.jpg");
        photo.save(&path).unwrap();
        assert_eq!(decode_image(&path).unwrap(), text);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_image_without_a_code_says_so() {
        let path = scratch("blank.png");
        image::GrayImage::from_pixel(200, 200, image::Luma([255]))
            .save(&path)
            .unwrap();
        let err = decode_image(&path).unwrap_err().to_string();
        assert!(err.contains("no readable QR code"), "{err}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn terminal_rendering_has_rows() {
        let text = render_terminal("HCWFD1:test").unwrap();
        assert!(text.lines().count() > 10);
    }
}
