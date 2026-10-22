//! Avatar uploads are images, judged on the bytes — never on the file name. A picture for a
//! person, a group or a server passes only when its `Content-Type` names one of the allowed
//! image types AND the first bytes carry that type's signature. One rule for every client.

use std::fmt;

/// The image types an avatar may be (D79 photos): the same list every picker offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Heic,
}

impl ImageKind {
    /// The kind a `Content-Type` header names; parameters (`; charset=…`) and case are
    /// ignored, the usual aliases (`image/jpg`, `image/heif`, `image/x-png`) are accepted.
    pub fn from_mime(content_type: &str) -> Option<Self> {
        let base = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "image/png" | "image/x-png" => Some(ImageKind::Png),
            "image/jpeg" | "image/jpg" | "image/pjpeg" => Some(ImageKind::Jpeg),
            "image/gif" => Some(ImageKind::Gif),
            "image/webp" => Some(ImageKind::Webp),
            "image/heic" | "image/heif" | "image/heic-sequence" | "image/heif-sequence" => {
                Some(ImageKind::Heic)
            }
            _ => None,
        }
    }
}

impl fmt::Display for ImageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ImageKind::Png => "PNG",
            ImageKind::Jpeg => "JPEG",
            ImageKind::Gif => "GIF",
            ImageKind::Webp => "WebP",
            ImageKind::Heic => "HEIC",
        })
    }
}

/// Human list of the allowed types, for error messages.
pub const ALLOWED_LIST: &str = "PNG, JPEG, HEIC, GIF or WebP";

/// How many leading bytes `sniff` needs to decide; callers reading from disk read this much.
pub const SNIFF_LEN: usize = 16;

/// The image type the bytes themselves announce, or `None` when they are not one of ours.
pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageKind::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageKind::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageKind::Gif);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind::Webp);
    }
    // ISO BMFF: a size, then `ftyp`, then the major brand. HEIF files from iPhones and Macs
    // announce `heic`/`heix` (still), `hevc`/`hevx` (sequence) or the structural brands
    // `mif1`/`msf1`.
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        let heif = [
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1",
            b"msf1", b"heif",
        ];
        if heif.iter().any(|b| *b == brand) {
            return Some(ImageKind::Heic);
        }
    }
    None
}

/// Why an upload is not an acceptable avatar; `Display` is the plain message a client shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAnImage {
    /// No `Content-Type` at all.
    MissingMime,
    /// A `Content-Type` outside the allowed list.
    BadMime(String),
    /// The bytes carry no known image signature.
    BadBytes,
    /// Header and bytes disagree (`image/png` header, JPEG bytes, …).
    Mismatch { header: ImageKind, bytes: ImageKind },
}

impl fmt::Display for NotAnImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotAnImage::MissingMime => {
                write!(
                    f,
                    "Pictures need a Content-Type; send a {ALLOWED_LIST} image"
                )
            }
            NotAnImage::BadMime(m) => {
                write!(f, "{m} is not an image; pictures must be {ALLOWED_LIST}")
            }
            NotAnImage::BadBytes => {
                write!(
                    f,
                    "That file is not an image; pictures must be {ALLOWED_LIST}"
                )
            }
            NotAnImage::Mismatch { header, bytes } => write!(
                f,
                "The file is {bytes} but was sent as {header}; pictures must be {ALLOWED_LIST}"
            ),
        }
    }
}

/// The gate every avatar endpoint runs: the declared type must be an allowed image, the
/// bytes must carry that same type's signature. Returns the agreed kind.
pub fn check_avatar(content_type: Option<&str>, bytes: &[u8]) -> Result<ImageKind, NotAnImage> {
    let header = match content_type.map(str::trim).filter(|s| !s.is_empty()) {
        None => return Err(NotAnImage::MissingMime),
        Some(ct) => ImageKind::from_mime(ct).ok_or_else(|| {
            NotAnImage::BadMime(ct.split(';').next().unwrap_or(ct).trim().to_owned())
        })?,
    };
    let kind = sniff(bytes).ok_or(NotAnImage::BadBytes)?;
    if kind != header {
        return Err(NotAnImage::Mismatch {
            header,
            bytes: kind,
        });
    }
    Ok(kind)
}

#[cfg(test)]
pub(crate) mod samples {
    //! Minimal byte heads for tests: real signatures, no pixel data needed to be judged.

    pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01";
    pub const JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
    ];
    pub const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00";
    pub const WEBP: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 \x18\x00\x00\x00";
    pub const HEIC: &[u8] = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic";
    /// The head of an ONNX model (a protobuf), as in the report that opened this work.
    pub const ONNX: &[u8] = b"\x08\x07\x12\x07pytorch\x1a\x052.1.0:\xe4\x03";
    pub const TEXT: &[u8] = b"hello, this is not a picture at all\n";
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;

    #[test]
    fn sniff_recognises_each_allowed_type_and_nothing_else() {
        assert_eq!(sniff(PNG), Some(ImageKind::Png));
        assert_eq!(sniff(JPEG), Some(ImageKind::Jpeg));
        assert_eq!(sniff(GIF), Some(ImageKind::Gif));
        assert_eq!(sniff(WEBP), Some(ImageKind::Webp));
        assert_eq!(sniff(HEIC), Some(ImageKind::Heic));
        assert_eq!(
            sniff(b"\x00\x00\x00\x1cftypmif1\x00\x00\x00\x00mif1heic"),
            Some(ImageKind::Heic),
            "structural HEIF brand"
        );
        assert_eq!(sniff(ONNX), None);
        assert_eq!(sniff(TEXT), None);
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"\x89PN"), None, "a truncated signature is not a PNG");
        assert_eq!(
            sniff(b"RIFF\x24\x00\x00\x00WAVEfmt "),
            None,
            "RIFF alone is not WebP"
        );
        assert_eq!(
            sniff(b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isom"),
            None,
            "an MP4 is ISO BMFF but not HEIF"
        );
        assert_eq!(
            sniff(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3"),
            None,
            "a PDF is not an image"
        );
    }

    #[test]
    fn mime_parsing_accepts_aliases_and_parameters() {
        assert_eq!(ImageKind::from_mime("image/jpeg"), Some(ImageKind::Jpeg));
        assert_eq!(ImageKind::from_mime("image/jpg"), Some(ImageKind::Jpeg));
        assert_eq!(
            ImageKind::from_mime("IMAGE/PNG; charset=binary"),
            Some(ImageKind::Png)
        );
        assert_eq!(ImageKind::from_mime("image/heif"), Some(ImageKind::Heic));
        assert_eq!(ImageKind::from_mime("application/octet-stream"), None);
        assert_eq!(ImageKind::from_mime("image/svg+xml"), None, "SVG is script");
        assert_eq!(ImageKind::from_mime("image/avif"), None);
        assert_eq!(ImageKind::from_mime(""), None);
    }

    #[test]
    fn check_needs_header_and_bytes_to_agree() {
        assert_eq!(check_avatar(Some("image/png"), PNG), Ok(ImageKind::Png));
        assert_eq!(
            check_avatar(Some("image/jpeg; q=1"), JPEG),
            Ok(ImageKind::Jpeg)
        );
        assert_eq!(check_avatar(Some("image/heif"), HEIC), Ok(ImageKind::Heic));

        // The report: an .onnx file. Whatever the header says, the bytes are not a picture.
        assert_eq!(
            check_avatar(Some("application/octet-stream"), ONNX),
            Err(NotAnImage::BadMime("application/octet-stream".into()))
        );
        assert_eq!(
            check_avatar(Some("image/png"), ONNX),
            Err(NotAnImage::BadBytes),
            "a renamed .onnx with an image header is still refused"
        );
        assert_eq!(check_avatar(None, PNG), Err(NotAnImage::MissingMime));
        assert_eq!(check_avatar(Some("  "), PNG), Err(NotAnImage::MissingMime));
        assert_eq!(
            check_avatar(Some("image/png"), JPEG),
            Err(NotAnImage::Mismatch {
                header: ImageKind::Png,
                bytes: ImageKind::Jpeg
            })
        );
        assert_eq!(
            check_avatar(Some("text/html"), b"<svg onload=alert(1)>"),
            Err(NotAnImage::BadMime("text/html".into()))
        );
    }

    #[test]
    fn messages_are_plain() {
        let msg = NotAnImage::BadBytes.to_string();
        assert!(msg.contains("not an image"), "{msg}");
        assert!(msg.contains("PNG, JPEG, HEIC, GIF or WebP"), "{msg}");
        let msg = NotAnImage::BadMime("application/octet-stream".into()).to_string();
        assert!(
            msg.starts_with("application/octet-stream is not an image"),
            "{msg}"
        );
        let msg = NotAnImage::Mismatch {
            header: ImageKind::Png,
            bytes: ImageKind::Jpeg,
        }
        .to_string();
        assert_eq!(
            msg,
            "The file is JPEG but was sent as PNG; pictures must be PNG, JPEG, HEIC, GIF or WebP"
        );
    }
}
