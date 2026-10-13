//! `fleetdeck settings get|set|secret` (DL-172, design 1.3): the settings
//! of the deck or of this machine's station, read and changed through the
//! running holder over its owner-only local socket. The holder is the only
//! writer of its config file while it runs, so the CLI never edits the file.
//!
//! A secret value is read from stdin (or a prompt that does not echo),
//! never from argv. It goes into one request and into no output.

use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::AsFd;

use anyhow::{Context, bail};
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::secret::Secret;
use fleetdeck_deck::local::{DeckRequest, DeckResponse};
use fleetdeck_proto::settings::{Apply, SecretSet, SettingsChange, SettingsSaved, SettingsView};
use fleetdeck_station::local::{LocalRequest, LocalResponse};
use serde_json::{Map, Value};

use crate::cli::SettingsCommand;
use crate::invite::{self, MAX_BYTES};

/// The code of a save that read an old revision.
const CHANGED: &str = "settings_changed";

/// Why a settings call failed.
#[derive(Debug)]
enum Failure {
    /// The holder answered with an error code and a fixed text.
    Settings { code: String, message: String },
    /// The socket or the answer failed.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl From<Failure> for anyhow::Error {
    fn from(f: Failure) -> Self {
        match f {
            Failure::Settings { code, message } => anyhow::anyhow!("{code}: {message}"),
            Failure::Other(e) => e,
        }
    }
}

/// A holder of settings behind a local socket.
trait Holder {
    async fn view(&self) -> Result<SettingsView, Failure>;
    async fn save(&self, change: SettingsChange) -> Result<SettingsSaved, Failure>;
    async fn secret(&self, secret: SecretSet, revision: String) -> Result<SettingsSaved, Failure>;
}

/// The deck, through `deck.sock`.
struct Deck<'a>(&'a NodeDirs);

/// The station of this machine, through `station.sock`.
struct Station<'a>(&'a NodeDirs);

fn unexpected(who: &str) -> Failure {
    Failure::Other(anyhow::anyhow!("unexpected answer from the {who}"))
}

impl Holder for Deck<'_> {
    async fn view(&self) -> Result<SettingsView, Failure> {
        match crate::deck::call(self.0, &DeckRequest::SettingsGet).await? {
            DeckResponse::Settings { view } => Ok(view),
            DeckResponse::SettingsError { code, message } => {
                Err(Failure::Settings { code, message })
            }
            _ => Err(unexpected("deck")),
        }
    }

    async fn save(&self, change: SettingsChange) -> Result<SettingsSaved, Failure> {
        match crate::deck::call(self.0, &DeckRequest::SettingsSet { change }).await? {
            DeckResponse::SettingsSaved { saved } => Ok(saved),
            DeckResponse::SettingsError { code, message } => {
                Err(Failure::Settings { code, message })
            }
            _ => Err(unexpected("deck")),
        }
    }

    async fn secret(&self, _: SecretSet, _: String) -> Result<SettingsSaved, Failure> {
        Err(Failure::Other(anyhow::anyhow!(
            "the deck has no secret settings; use --station"
        )))
    }
}

impl Holder for Station<'_> {
    async fn view(&self) -> Result<SettingsView, Failure> {
        match crate::station::call(self.0, &LocalRequest::SettingsGet).await? {
            LocalResponse::Settings { view } => Ok(view),
            LocalResponse::SettingsError { code, message } => {
                Err(Failure::Settings { code, message })
            }
            _ => Err(unexpected("station")),
        }
    }

    async fn save(&self, change: SettingsChange) -> Result<SettingsSaved, Failure> {
        match crate::station::call(self.0, &LocalRequest::SettingsSet { change }).await? {
            LocalResponse::SettingsSaved { saved } => Ok(saved),
            LocalResponse::SettingsError { code, message } => {
                Err(Failure::Settings { code, message })
            }
            _ => Err(unexpected("station")),
        }
    }

    async fn secret(&self, secret: SecretSet, revision: String) -> Result<SettingsSaved, Failure> {
        let request = LocalRequest::SettingsSecret { secret, revision };
        match crate::station::call(self.0, &request).await? {
            LocalResponse::SettingsSaved { saved } => Ok(saved),
            LocalResponse::SettingsError { code, message } => {
                Err(Failure::Settings { code, message })
            }
            _ => Err(unexpected("station")),
        }
    }
}

/// `<value>` of `settings set`: JSON when it parses (`true`, `5`, `["a"]`,
/// `null`), else the plain string.
fn parse_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

/// `fleetdeck settings ...`.
pub async fn run(dirs: &NodeDirs, command: &SettingsCommand) -> anyhow::Result<()> {
    match command {
        SettingsCommand::Get { station, json } => {
            let view = if *station {
                Station(dirs).view().await
            } else {
                Deck(dirs).view().await
            }?;
            println!("{}", render_view(&view, *json)?);
            Ok(())
        }
        SettingsCommand::Set {
            key,
            value,
            station,
        } => {
            let value = parse_value(value);
            let saved = if *station {
                set(&Station(dirs), key, value).await
            } else {
                set(&Deck(dirs), key, value).await
            }?;
            println!("{}", render_saved(&saved));
            Ok(())
        }
        SettingsCommand::Secret { key, station } => {
            if !*station {
                bail!("the deck has no secret settings; use --station");
            }
            let value = read_value()?;
            secret(&Station(dirs), key, &value).await?;
            drop(value);
            println!("Saved.");
            Ok(())
        }
    }
}

/// Reads the current revision, then saves the change with it. A concurrent
/// change gives `settings_changed`; the call then reads again and tries
/// once more.
///
/// # Errors
/// The failure of the holder.
async fn set(holder: &impl Holder, key: &str, value: Value) -> Result<SettingsSaved, Failure> {
    let mut attempt = 0;
    loop {
        let revision = holder.view().await?.revision;
        let mut changes = Map::new();
        changes.insert(key.to_owned(), value.clone());
        let change = SettingsChange {
            changes,
            revision,
            elevation: None,
        };
        match holder.save(change).await {
            Err(Failure::Settings { code, .. }) if code == CHANGED && attempt == 0 => {
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// As [`set`], for a secret value.
///
/// # Errors
/// The failure of the holder.
async fn secret(
    holder: &impl Holder,
    key: &str,
    value: &Secret<String>,
) -> Result<SettingsSaved, Failure> {
    let mut attempt = 0;
    loop {
        let revision = holder.view().await?.revision;
        let set = SecretSet {
            key: key.to_owned(),
            value: value.expose().clone(),
        };
        match holder.secret(set, revision).await {
            Err(Failure::Settings { code, .. }) if code == CHANGED && attempt == 0 => {
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// The text of a saved change.
fn render_saved(saved: &SettingsSaved) -> String {
    let mut out = if saved.restart_needed.is_empty() {
        "Saved and applied.".to_owned()
    } else {
        format!(
            "Saved. Applies after a restart: {}.",
            saved.restart_needed.join(", ")
        )
    };
    if !saved.unapplied.is_empty() {
        let _ = write!(out, " Not applied: {}.", saved.unapplied.join(", "));
    }
    out
}

/// The `--json` text, or the table of key, value and apply.
///
/// # Errors
/// The view cannot be written as JSON.
fn render_view(view: &SettingsView, json: bool) -> anyhow::Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(view)?);
    }
    let rows: Vec<[String; 3]> = view
        .fields
        .iter()
        .map(|f| {
            let value = view.values.get(&f.key).map_or_else(|| "-".to_owned(), show);
            let apply = match f.apply {
                Apply::Live => "live",
                Apply::Restart => "restart",
            };
            [f.key.clone(), value, apply.to_owned()]
        })
        .collect();
    let head = ["key".to_owned(), "value".to_owned(), "apply".to_owned()];
    let width = |i: usize| {
        rows.iter()
            .chain(std::iter::once(&head))
            .map(|r| r[i].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(0), width(1));
    let line = |r: &[String; 3]| format!("{:<w0$}  {:<w1$}  {}", r[0], r[1], r[2]);
    let mut out = vec![line(&head).trim_end().to_owned()];
    out.extend(rows.iter().map(|r| line(r).trim_end().to_owned()));
    Ok(out.join("\n"))
}

/// A value for the table: text as it is, a secret entry as `(set)`, the
/// rest as compact JSON.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => hide_secrets(other).to_string(),
    }
}

/// Replaces every `{ "set": true, ... }` entry by `"(set)"`.
fn hide_secrets(v: &Value) -> Value {
    match v {
        Value::Object(o) if o.get("set") == Some(&Value::Bool(true)) => {
            Value::String("(set)".to_owned())
        }
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), hide_secrets(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(hide_secrets).collect()),
        other => other.clone(),
    }
}

/// The secret value: from a prompt that does not echo when stdin is a
/// terminal, else the first line of stdin.
fn read_value() -> anyhow::Result<Secret<String>> {
    let stdin = io::stdin();
    if rustix::termios::isatty(stdin.as_fd()) {
        return invite::prompt("Value (not shown): ", "value", "pipe the value to stdin");
    }
    value_line(stdin.lock())
}

/// The first line of `r` with its line end cut, at most [`MAX_BYTES`]. No
/// error contains the text.
///
/// # Errors
/// No line, an empty line, or a line that is too long.
fn value_line(r: impl Read) -> anyhow::Result<Secret<String>> {
    let mut raw = String::new();
    BufReader::new(r.take(MAX_BYTES))
        .read_line(&mut raw)
        .context("cannot read the value")?;
    let line = Secret::new(raw);
    let text = line.expose();
    let cut = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if cut.is_empty() {
        bail!("no value was given");
    }
    if text.len() as u64 >= MAX_BYTES && !text.ends_with('\n') {
        bail!("the value is too long");
    }
    Ok(Secret::new(cut.to_owned()))
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
