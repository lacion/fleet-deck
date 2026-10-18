//! `fleetdeck omarchy theme-sync` (R13 P4 task F1, `omarchy-p4.md` §3.7): sends
//! the Omarchy theme palette of this machine to the deck.
//!
//! It reads `$HOME/.local/state/omarchy/current/theme/fleetdeck.json`, the
//! file that the Omarchy theme template `fleetdeck.json.tpl` generates, and
//! calls the local station (`LocalRequest::PaletteSet`). The station keeps
//! the palette in memory and sends it to the deck.
//!
//! - A missing file, or a file that is not usable, is not an error: the
//!   command prints one line on stderr, sends nothing and exits 0. The theme
//!   hook of Omarchy runs it, and the previous palette stays.
//! - A deck that is not connected exits 1: the station sends the palette
//!   again when the link is up.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, bail};
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_proto::palette::{PaletteColors, PaletteMode, StationPalette};
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::local_palette::palette_valid;
use serde::Deserialize;

use super::STATION_LIMIT;

/// The file, below `$HOME`.
const THEME_FILE: &str = ".local/state/omarchy/current/theme/fleetdeck.json";
/// The largest theme file, in bytes (4 KiB).
const MAX_FILE_BYTES: u64 = 4096;

const MISSING: &str = "Fleet Deck: no generated theme file; nothing was sent.";
const UNUSABLE: &str = "Fleet Deck: the theme file is not usable; nothing was sent.";
const NOT_CONNECTED: &str =
    "The deck is not connected. The palette is sent when the link is up again.";
const NO_STATION: &str = "The local station is not running.";

/// The generated file: `mode` and the ten colors in one flat object. A key
/// that is missing, extra or repeated is a parse error.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    mode: PaletteMode,
    background: String,
    foreground: String,
    accent: String,
    muted: String,
    dark_background: String,
    dark_foreground: String,
    bright_foreground: String,
    selection: String,
    red: String,
    yellow: String,
}

/// What the file holds.
#[derive(Debug, PartialEq, Eq)]
enum Theme {
    Missing,
    Unusable,
    Palette(Box<StationPalette>),
}

/// The palette of the text of the file, or `None` when it is not usable: the
/// text still holds `{{`, is not the flat object, or a color is not
/// `#rrggbb`.
fn parse(text: &str) -> Option<StationPalette> {
    if text.contains("{{") {
        return None;
    }
    let file: ThemeFile = serde_json::from_str(text).ok()?;
    let palette = StationPalette {
        v: 1,
        mode: file.mode,
        colors: PaletteColors {
            background: file.background,
            foreground: file.foreground,
            accent: file.accent,
            muted: file.muted,
            dark_background: file.dark_background,
            dark_foreground: file.dark_foreground,
            bright_foreground: file.bright_foreground,
            selection: file.selection,
            red: file.red,
            yellow: file.yellow,
        },
    };
    palette_valid(&palette).then_some(palette)
}

/// Reads at most [`MAX_FILE_BYTES`] + 1 bytes of `path`.
fn read(path: &Path) -> Theme {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Theme::Missing,
        Err(_) => return Theme::Unusable,
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_FILE_BYTES
    {
        return Theme::Unusable;
    }
    match std::str::from_utf8(&bytes).ok().and_then(parse) {
        Some(palette) => Theme::Palette(Box::new(palette)),
        None => Theme::Unusable,
    }
}

fn theme_path(home: &Path) -> PathBuf {
    home.join(THEME_FILE)
}

/// One `palette_set` call to the local station, at most [`STATION_LIMIT`].
async fn call(dirs: &NodeDirs, palette: StationPalette) -> anyhow::Result<LocalResponse> {
    let work = async {
        let mut client = LocalClient::connect(&socket_path(&dirs.station()))
            .await
            .map_err(|_| anyhow!(NO_STATION))?;
        client
            .call(&LocalRequest::PaletteSet { palette })
            .await
            .map_err(|_| anyhow!(NO_STATION))
    };
    tokio::time::timeout(STATION_LIMIT, work)
        .await
        .unwrap_or_else(|_| Err(anyhow!("The local station did not answer in 5 s.")))
}

/// `fleetdeck omarchy theme-sync`.
///
/// # Errors
/// `HOME` is not set; the local station is not running or does not answer;
/// the station refuses the palette.
pub async fn run(dirs: &NodeDirs) -> anyhow::Result<ExitCode> {
    let Some(home) = Env::from_process().home else {
        bail!("HOME is not set, so the theme file is not known");
    };
    let palette = match read(&theme_path(&home)) {
        Theme::Palette(palette) => *palette,
        Theme::Missing => {
            eprintln!("{MISSING}");
            return Ok(ExitCode::SUCCESS);
        }
        Theme::Unusable => {
            eprintln!("{UNUSABLE}");
            return Ok(ExitCode::SUCCESS);
        }
    };
    match call(dirs, palette).await? {
        LocalResponse::PaletteStored { sent: true } => Ok(ExitCode::SUCCESS),
        LocalResponse::PaletteStored { sent: false } => {
            eprintln!("{NOT_CONNECTED}");
            Ok(ExitCode::from(1))
        }
        LocalResponse::Error { message } => bail!("the station refused the palette: {message}"),
        _ => bail!("the station gave an answer that this program does not know"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r##"{
  "mode": "dark",
  "background": "#1a1b26",
  "foreground": "#C0CAF5",
  "accent": "#7aa2f7",
  "muted": "#565f89",
  "dark_background": "#16161e",
  "dark_foreground": "#a9b1d6",
  "bright_foreground": "#c0caf5",
  "selection": "#283457",
  "red": "#f7768e",
  "yellow": "#e0af68"
}"##;

    #[test]
    fn a_good_file_gives_a_version_1_palette_with_the_colors_as_written() {
        let p = parse(GOOD).unwrap();
        assert_eq!(p.v, 1);
        assert_eq!(p.mode, PaletteMode::Dark);
        assert_eq!(p.colors.foreground, "#C0CAF5");
        assert_eq!(p.colors.yellow, "#e0af68");
        let light = parse(&GOOD.replace("\"dark\"", "\"light\"")).unwrap();
        assert_eq!(light.mode, PaletteMode::Light);
    }

    #[test]
    fn a_file_that_is_not_usable_gives_none() {
        let bad = [
            // The template still holds a mapping.
            GOOD.replace("#7aa2f7", "{{ accent }}"),
            "{{".to_owned(),
            String::new(),
            "not json".to_owned(),
            "[]".to_owned(),
            GOOD.replace("\"dark\"", "\"blue\""),
            GOOD.replace("\"dark\"", "\"Dark\""),
            GOOD.replace("#7aa2f7", "#7aa2f"),
            GOOD.replace("#7aa2f7", "7aa2f7"),
            GOOD.replace("#7aa2f7", "#7aa2fg"),
            GOOD.replace("#7aa2f7", "#7aa2f7f"),
            GOOD.replace("#7aa2f7", "red"),
            // A key is missing.
            GOOD.replace("  \"red\": \"#f7768e\",\n", ""),
            // A key is extra.
            GOOD.replace("\"mode\"", "\"color3\": \"#000000\", \"mode\""),
            GOOD.replace("\"mode\"", "\"v\": 1, \"mode\""),
            // A key is repeated.
            GOOD.replace("\"mode\"", "\"red\": \"#000000\", \"mode\""),
            // A color is not a string.
            GOOD.replace("\"#7aa2f7\"", "7"),
        ];
        for text in &bad {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn the_file_cases_of_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleetdeck.json");
        assert_eq!(read(&path), Theme::Missing);
        std::fs::write(&path, GOOD).unwrap();
        assert!(matches!(read(&path), Theme::Palette(_)));
        std::fs::write(&path, "{{").unwrap();
        assert_eq!(read(&path), Theme::Unusable);
        // Exactly 4 KiB is read; one byte more is not usable.
        let pad = |n: usize| format!("{}{}", GOOD, " ".repeat(n - GOOD.len()));
        std::fs::write(&path, pad(4096)).unwrap();
        assert!(matches!(read(&path), Theme::Palette(_)));
        std::fs::write(&path, pad(4097)).unwrap();
        assert_eq!(read(&path), Theme::Unusable);
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read(&path), Theme::Unusable);
        // A directory is not a theme file.
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(read(&sub), Theme::Unusable);
    }

    #[test]
    fn the_theme_file_is_below_the_home() {
        assert_eq!(
            theme_path(Path::new("/home/me")),
            Path::new("/home/me/.local/state/omarchy/current/theme/fleetdeck.json")
        );
    }
}
