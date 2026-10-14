//! WSL: the Windows logon task that starts the distro (design service.md §6).
//!
//! The only variable part of a script is the checked distro name, written
//! between single quotes. A failure here is a warning, never an error.

use std::path::PathBuf;
use std::time::Duration;

use super::Context;
use super::Deps;
use super::runner::RunError;

/// The time limit of one PowerShell call.
const LIMIT: Duration = Duration::from_secs(30);

/// The PowerShell arguments: the script comes on stdin.
const ARGS: [&str; 6] = [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

/// The success line of the install.
pub const ADDED: &str = "Windows logon task: added (WSL starts when you sign in to Windows).";

/// True for `^[A-Za-z0-9._-]{1,64}$`.
pub fn distro_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The lines that open every script: the task name from the distro and the
/// Windows account.
fn head(distro: &str) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $id = [Security.Principal.WindowsIdentity]::GetCurrent()\n\
         $name = 'WSL {distro} ' + $id.User.Value\n"
    )
}

/// The install script (design §6).
pub fn install_script(distro: &str) -> String {
    let mut script = head(distro);
    script.push_str(&format!(
        "$sys = $env:SystemRoot + '\\System32'\n\
         $a = New-ScheduledTaskAction -Execute ($sys + '\\conhost.exe') -Argument ('--headless \"' + $sys + '\\wsl.exe\" -d {distro} --exec /bin/sleep infinity')\n\
         $logon = New-ScheduledTaskTrigger -AtLogOn -User $id.Name\n\
         $again = New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Minutes 5) -RepetitionDuration (New-TimeSpan -Days 3650)\n\
         $p = New-ScheduledTaskPrincipal -UserId $id.User.Value -LogonType Interactive -RunLevel Limited\n\
         $s = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)\n\
         Register-ScheduledTask -TaskPath '\\Fleet Deck\\' -TaskName $name -Action $a -Trigger @($logon, $again) -Principal $p -Settings $s -Force | Out-Null\n\
         Start-ScheduledTask -TaskPath '\\Fleet Deck\\' -TaskName $name\n\
         'ok'\n"
    ));
    script
}

/// The status script: `absent`, or `state=<State>`.
pub fn status_script(distro: &str) -> String {
    let mut script = head(distro);
    script.push_str(
        "$t = Get-ScheduledTask -TaskPath '\\Fleet Deck\\' -TaskName $name -ErrorAction SilentlyContinue\n\
         if ($t) { 'state=' + $t.State } else { 'absent' }\n",
    );
    script
}

/// The remove script.
pub fn remove_script(distro: &str) -> String {
    let mut script = head(distro);
    script.push_str(
        "if (Get-ScheduledTask -TaskPath '\\Fleet Deck\\' -TaskName $name -ErrorAction SilentlyContinue) { Unregister-ScheduledTask -TaskPath '\\Fleet Deck\\' -TaskName $name -Confirm:$false }\n\
         'ok'\n",
    );
    script
}

/// The checked distro and the PowerShell program, or the reason that the
/// task is skipped.
fn target(ctx: &Context) -> Result<(String, PathBuf), String> {
    let distro = ctx
        .distro
        .as_deref()
        .ok_or_else(|| "WSL_DISTRO_NAME is not set".to_owned())?;
    if !distro_ok(distro) {
        return Err("the distro name has other characters".to_owned());
    }
    let program = ctx
        .powershell
        .clone()
        .ok_or_else(|| "the Windows PowerShell program did not pass the path checks".to_owned())?;
    Ok((distro.to_owned(), program))
}

/// The reason text of a PowerShell call that did not succeed.
fn reason(program: &str, result: &Result<super::runner::Output, RunError>) -> String {
    match result {
        Ok(out) => out
            .stderr_line()
            .map_or_else(|| out.status_text(), str::to_owned),
        Err(e) => e.text(program, LIMIT),
    }
}

fn call(ctx: &Context, deps: &dyn Deps, script: &str) -> Result<super::runner::Output, String> {
    let (_, program) = target(ctx)?;
    let program = program.to_string_lossy().into_owned();
    let args: Vec<String> = ARGS
        .iter()
        .map(|a| (*a).to_owned())
        .chain(["-".to_owned()])
        .collect();
    let result = deps.run(&program, &args, Some(script), LIMIT);
    match result {
        Ok(out) if out.success() => Ok(out),
        other => Err(reason(&program, &other)),
    }
}

/// The warning of a task that was not added.
pub fn add_warning(reason: &str) -> String {
    format!(
        "The services start when WSL starts. Fleet Deck could not add the Windows logon task ({reason})."
    )
}

/// The warning of a task that was not removed.
pub fn remove_warning(reason: &str) -> String {
    format!(
        "Fleet Deck could not remove the Windows logon task ({reason}). \
         Remove the task folder \"Fleet Deck\" in the Windows Task Scheduler."
    )
}

/// Adds and starts the task; the line for the summary.
pub fn install(ctx: &Context, deps: &dyn Deps) -> String {
    let done = target(ctx).and_then(|(distro, _)| {
        let out = call(ctx, deps, &install_script(&distro))?;
        if out.last_line() == Some("ok") {
            Ok(())
        } else {
            Err(out
                .stderr_line()
                .map_or_else(|| out.status_text(), str::to_owned))
        }
    });
    match done {
        Ok(()) => ADDED.to_owned(),
        Err(reason) => add_warning(&reason),
    }
}

/// Removes the task; `Err` is the warning text.
pub fn remove(ctx: &Context, deps: &dyn Deps) -> Result<(), String> {
    let done = target(ctx).and_then(|(distro, _)| {
        let out = call(ctx, deps, &remove_script(&distro))?;
        if out.last_line() == Some("ok") {
            Ok(())
        } else {
            Err(out
                .stderr_line()
                .map_or_else(|| out.status_text(), str::to_owned))
        }
    });
    done.map_err(|reason| remove_warning(&reason))
}

/// The state of the task: `absent`, its state (for example `Running`) or
/// `unknown`.
pub fn status(ctx: &Context, deps: &dyn Deps) -> String {
    let answer = target(ctx).and_then(|(distro, _)| call(ctx, deps, &status_script(&distro)));
    match answer {
        Ok(out) => match out.last_line() {
            Some("absent") => "absent".to_owned(),
            Some(line) => match line.strip_prefix("state=") {
                Some(state) if !state.is_empty() => state.to_owned(),
                _ => "unknown".to_owned(),
            },
            None => "unknown".to_owned(),
        },
        Err(_) => "unknown".to_owned(),
    }
}

/// The `--dry-run` line: the call, or the warning when the task is skipped.
pub fn dry_line(ctx: &Context) -> String {
    match target(ctx) {
        Ok((distro, program)) => format!(
            "$ {} {} -   (registers and starts the Windows logon task \"\\Fleet Deck\\WSL {distro} <SID>\")",
            program.display(),
            ARGS.join(" ")
        ),
        Err(reason) => add_warning(&reason),
    }
}
