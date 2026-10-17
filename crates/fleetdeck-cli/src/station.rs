//! `fleetdeck station serve|sessions|run|approve|interrupt` (M2 P2-7).
//!
//! `serve` runs the station; the other commands are clients of its local
//! socket (`fleetdeck_station::local`), so they work without the deck
//! (PLAN §6.5).

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::command::{
    ApprovalDecision, Receipt, ReceiptState, SessionCommand, SessionSpec, UserInput,
};
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};

use crate::cli::{ApproveArgs, RunArgs};
use crate::restart::{RestartHandle, stop_or_restart};

/// `fleetdeck station serve`: runs until SIGINT or SIGTERM, or until a
/// restart is requested (DL-172).
pub async fn serve(dirs: &NodeDirs, env: &Env, restart: &RestartHandle) -> anyhow::Result<()> {
    let path = dirs.station_config();
    let config = StationConfig::load_with_store(&path, env, &dirs.station().join("secrets"))?;
    fleetdeck_station::serve::serve(
        dirs,
        &config,
        Some(Arc::new(restart.clone())),
        stop_or_restart(restart),
    )
    .await?;
    Ok(())
}

pub(crate) async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "no SIGTERM handler");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

pub(crate) async fn call(dirs: &NodeDirs, request: &LocalRequest) -> anyhow::Result<LocalResponse> {
    let path = socket_path(&dirs.station());
    let mut client = LocalClient::connect(&path).await?;
    let response = client.call(request).await?;
    if let LocalResponse::Error { message } = &response {
        bail!("the station refused the request: {message}");
    }
    Ok(response)
}

/// Accepted → the receipt; rejected → an error with the reject code.
fn accepted(response: LocalResponse) -> anyhow::Result<Receipt> {
    let LocalResponse::Receipt { receipt } = response else {
        bail!("unexpected answer from the station: {response:?}");
    };
    if receipt.state == ReceiptState::Rejected {
        let (code, message) = receipt.reject.as_ref().map_or_else(
            || ("unknown".to_owned(), String::new()),
            |r| (word(&r.code), r.message.clone()),
        );
        bail!("rejected ({code}): {message}");
    }
    Ok(receipt)
}

fn command(command: SessionCommand) -> LocalRequest {
    LocalRequest::Command {
        command_id: None,
        command,
    }
}

/// `fleetdeck station sessions [--json]`.
pub async fn sessions(dirs: &NodeDirs, json: bool) -> anyhow::Result<()> {
    let response = call(dirs, &LocalRequest::Sessions).await?;
    let LocalResponse::Sessions { sessions } = response else {
        bail!("unexpected answer from the station: {response:?}");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
    } else if sessions.is_empty() {
        println!("no sessions");
    } else {
        for s in &sessions {
            print!("{}", render(s));
        }
    }
    Ok(())
}

/// The text form of one session: a line, then one line per open request.
#[must_use]
pub fn render(s: &SessionSummary) -> String {
    let mut out = format!(
        "{}  {}  {}  {}  {}\n",
        s.session_id,
        s.provider_instance.as_ref().map_or("-", |p| p.as_str()),
        word(&s.activity),
        word(&s.state),
        s.cwd.as_deref().unwrap_or("-"),
    );
    for r in &s.open_requests {
        let options: Vec<&str> = r.options.iter().map(|o| o.option_id.as_str()).collect();
        let _ = writeln!(
            out,
            "  request {}  {}: {}  options: {}",
            r.request_id,
            r.title,
            r.details,
            options.join(", ")
        );
    }
    out
}

/// The serde name of a unit enum value (`needs_you`, `running`).
fn word<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

/// `fleetdeck station run --provider P --cwd DIR [--mode M] PROMPT`:
/// prints the new session id.
pub async fn run(dirs: &NodeDirs, args: &RunArgs) -> anyhow::Result<()> {
    let cwd = absolute(&args.cwd)?;
    let response = call(
        dirs,
        &command(SessionCommand::Start {
            spec: SessionSpec {
                provider_instance: args.provider.clone().into(),
                cwd,
                mode: args.mode.clone(),
                model: None,
                title: None,
                coordinator: None,
                launch: None,
            },
            prompt: Some(UserInput::new(args.prompt.clone())),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        }),
    )
    .await?;
    let receipt = accepted(response)?;
    let session = receipt
        .result
        .and_then(|r| r.session_id)
        .context("the station accepted the start but sent no session id")?;
    println!("{session}");
    Ok(())
}

fn absolute(path: &Path) -> anyhow::Result<String> {
    let path = std::path::absolute(path)
        .with_context(|| format!("working directory `{}`", path.display()))?;
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("`{}` is not UTF-8", path.display()))
}

/// `fleetdeck station approve REQUEST [--option ID]`.
pub async fn approve(dirs: &NodeDirs, args: &ApproveArgs) -> anyhow::Result<()> {
    let response = call(
        dirs,
        &LocalRequest::Approve {
            command_id: None,
            request_id: args.request.clone().into(),
            decision: ApprovalDecision {
                answered_by: None,
                option_id: args.option.clone(),
                updated_input: None,
                text: None,
            },
        },
    )
    .await?;
    accepted(response)?;
    println!("answered {} with {}", args.request, args.option);
    Ok(())
}

/// `fleetdeck station interrupt SESSION`.
pub async fn interrupt(dirs: &NodeDirs, session: &str) -> anyhow::Result<()> {
    let response = call(
        dirs,
        &command(SessionCommand::Interrupt {
            session_id: session.to_owned().into(),
        }),
    )
    .await?;
    accepted(response)?;
    println!("interrupt sent to {session}");
    Ok(())
}
