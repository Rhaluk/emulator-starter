use std::borrow::Cow;
use std::sync::OnceLock;

use cosmic::widget::icon::{self, Handle};

const ANDROID: &[u8] = include_bytes!("../images/android(1).svg");
const PLAY: &[u8] = include_bytes!("../images/play.svg");
const STOP: &[u8] = include_bytes!("../images/stop.svg");

pub fn android() -> Handle {
    symbolic(android_bytes())
}

pub fn play() -> Handle {
    symbolic(play_bytes())
}

pub fn stop() -> Handle {
    symbolic(stop_bytes())
}

fn symbolic(bytes: &'static [u8]) -> Handle {
    icon::from_svg_bytes(Cow::Borrowed(bytes)).symbolic(true)
}

fn android_bytes() -> &'static [u8] {
    static CLEAN: OnceLock<Vec<u8>> = OnceLock::new();
    CLEAN.get_or_init(|| strip_inline_color(ANDROID)).as_slice()
}

fn play_bytes() -> &'static [u8] {
    static CLEAN: OnceLock<Vec<u8>> = OnceLock::new();
    CLEAN.get_or_init(|| strip_inline_color(PLAY)).as_slice()
}

fn stop_bytes() -> &'static [u8] {
    static CLEAN: OnceLock<Vec<u8>> = OnceLock::new();
    CLEAN.get_or_init(|| strip_inline_color(STOP)).as_slice()
}

/// Drop hardcoded `style="color: …"` so symbolic icons follow the COSMIC theme.
pub fn strip_inline_color(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_ref();
    while let Some(start) = rest.find(" style=\"") {
        out.push_str(&rest[..start]);
        let after = &rest[start + " style=\"".len()..];
        match after.find('"') {
            Some(end) => rest = &after[end + 1..],
            None => {
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out.into_bytes()
}
