//! One file panel for every "choose a picture" crayon: own profile, group, server. On macOS
//! it is an `NSOpenPanel` limited to image types, so every other file is greyed out and
//! cannot be chosen; elsewhere gpui's generic prompt plus the same extension filter.

use std::path::{Path, PathBuf};

use futures::channel::oneshot;
use gpui::App;

/// What a picture may be (the relay refuses anything else by MIME and magic bytes).
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "heic", "heif", "gif", "webp"];

/// Judged on the extension only; the relay judges the bytes.
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// Opens the picker. Resolves to `Some(path)` when a picture was chosen, `None` on Cancel
/// (or when what came back is not an image).
pub fn pick_image(cx: &mut App, prompt: &str) -> oneshot::Receiver<Option<PathBuf>> {
    let (tx, rx) = oneshot::channel();
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        macos::open(prompt, tx);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.to_owned().into()),
        });
        cx.spawn(async move |_| {
            let picked = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next().filter(|p| is_image_path(p)),
                _ => None,
            };
            let _ = tx.send(picked);
        })
        .detach();
    }
    rx
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::path::PathBuf;

    use block2::RcBlock;
    use futures::channel::oneshot;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSModalResponse, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString};
    use objc2_uniform_type_identifiers::{
        UTType, UTTypeGIF, UTTypeHEIC, UTTypeJPEG, UTTypePNG, UTTypeWebP,
    };

    use super::is_image_path;

    /// The list as shown to people, under the panel's title.
    const ALLOWED_LIST: &str = "PNG, JPEG, HEIC, GIF or WebP";

    /// An `NSOpenPanel` that only lets image files be chosen, as a sheet on the key window
    /// when there is one (the picker the report's screenshot shows), modeless otherwise.
    pub(super) fn open(prompt: &str, tx: oneshot::Sender<Option<PathBuf>>) {
        let Some(mtm) = MainThreadMarker::new() else {
            let _ = tx.send(None);
            return;
        };
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setResolvesAliases(true);
        panel.setTreatsFilePackagesAsDirectories(false);
        let types: [&UTType; 5] =
            unsafe { [UTTypePNG, UTTypeJPEG, UTTypeHEIC, UTTypeGIF, UTTypeWebP] };
        panel.setAllowedContentTypes(&NSArray::from_slice(&types));
        panel.setPrompt(Some(&NSString::from_str(prompt)));
        panel.setMessage(Some(&NSString::from_str(&format!(
            "Choose a picture ({ALLOWED_LIST})."
        ))));

        let tx = Cell::new(Some(tx));
        let chosen = panel.clone();
        let handler = RcBlock::new(move |response: NSModalResponse| {
            let path = if response == NSModalResponseOK {
                chosen
                    .URL()
                    .and_then(|url| url.to_file_path())
                    .filter(|p| is_image_path(p))
            } else {
                None
            };
            if let Some(tx) = tx.take() {
                let _ = tx.send(path);
            }
        });
        match NSApplication::sharedApplication(mtm).keyWindow() {
            Some(window) => panel.beginSheetModalForWindow_completionHandler(&window, &handler),
            None => panel.beginWithCompletionHandler(&handler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_image_extensions_pass() {
        for ok in [
            "a.png", "b.JPG", "c.jpeg", "d.heic", "e.HEIF", "f.gif", "g.webp",
        ] {
            assert!(is_image_path(Path::new(ok)), "{ok}");
        }
        for bad in [
            "code_submission_v1.onnx",
            "notes.txt",
            "archive.zip",
            "poster.pdf",
            "image",
            "x.svg",
            ".png",
        ] {
            assert!(!is_image_path(Path::new(bad)), "{bad}");
        }
    }
}
