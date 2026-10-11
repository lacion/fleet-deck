//! PLAN §6.2 capability-matrix cells for Codex that no other test names
//! directly: fork, child work with a parent link, background work after the
//! turn, live deltas, terminal status quality, native ids, and usage. Each
//! test name is cited in the matrix and in ADR-002.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use s2_codex::replay::{ReplayReport, replay};
use serde_json::Value;
use spike_canon::fixture::{Fixture, Record};
use spike_canon::{CanonEvent, ToolKind, ToolStatus, TurnStatus};

fn dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load(name: &str) -> Fixture {
    Fixture::load(&dir().join(format!("{name}.jsonl"))).unwrap()
}

fn run(name: &str) -> ReplayReport {
    replay(&load(name)).unwrap()
}

fn threads(r: &ReplayReport) -> Vec<String> {
    r.events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::SessionStarted { provider_refs, .. } => provider_refs
                .get("thread_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn last_message(r: &ReplayReport) -> String {
    r.events
        .iter()
        .rev()
        .find_map(|e| match e {
            CanonEvent::MessageCompleted { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap()
}

fn out_methods(fx: &Fixture) -> Vec<String> {
    fx.records
        .iter()
        .filter_map(|r| match r {
            Record::Out { frame } => frame["method"].as_str().map(str::to_owned),
            _ => None,
        })
        .collect()
}

/// Fork: `thread/fork` of a finished thread gives a new thread id that keeps
/// the source's context.
#[test]
fn fork_gives_a_new_thread_with_the_old_context() {
    let fx = load("fork_thread");
    let forked = fx.records.iter().any(|r| match r {
        Record::In { frame } => frame["method"] == "thread/fork",
        _ => false,
    });
    assert!(forked, "the second process forks");
    let r = replay(&fx).unwrap();
    let ids = threads(&r);
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "the fork is a new thread");
    assert!(last_message(&r).to_lowercase().contains("maple"));
}

/// Child work: a sub-agent is a `collabAgentToolCall` item (`spawnAgent`,
/// then `wait`) of the parent thread. Codex streams the child thread's own
/// turn and items on the same connection with the child's `threadId`, and the
/// completed `spawnAgent` item names that thread in `receiverThreadIds`. The
/// core links child tool items to the `spawnAgent` node
/// (`spawn_agent_links_its_child_thread_when_the_item_completes`); child turns
/// never end the parent turn.
#[test]
fn subagents_appear_as_tool_calls_of_the_parent_thread() {
    let fx = load("subagent");
    let r = replay(&fx).unwrap();
    let sub: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::ToolStarted {
                tool_kind: ToolKind::Subagent,
                title,
                parent_node_id,
                ..
            } => Some((title.clone(), parent_node_id.clone())),
            _ => None,
        })
        .collect();
    let titles: Vec<_> = sub.iter().map(|(t, _)| t.as_str()).collect();
    assert!(titles.contains(&"spawnAgent"), "{titles:?}");
    assert!(sub.iter().all(|(_, p)| p.is_none()), "top-level tool calls");
    let own = threads(&r).remove(0);
    let mut children = Vec::new();
    let mut child_frames = Vec::new();
    for rec in &fx.records {
        let Record::Out { frame } = rec else { continue };
        let p = &frame["params"];
        let item = &p["item"];
        if frame["method"] == "item/completed" && item["tool"] == "spawnAgent" {
            children.extend(
                item["receiverThreadIds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t.as_str().unwrap().to_owned()),
            );
        }
        if let Some(t) = p["threadId"].as_str()
            && t != own
            && matches!(
                frame["method"].as_str(),
                Some("turn/started" | "item/completed")
            )
        {
            child_frames.push(t.to_owned());
        }
    }
    assert_eq!(children.len(), 1, "one child thread");
    assert!(
        !child_frames.is_empty(),
        "the child thread streams its turn"
    );
    assert!(child_frames.iter().all(|t| *t == children[0]));
    let turns = r
        .events
        .iter()
        .filter(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .count();
    assert_eq!(turns, 1, "the child turn does not end the parent turn");
    assert!(last_message(&r).contains("sub-ok"));
}

/// Background work: no notification announces a command that outlives the
/// turn. The host polls `thread/backgroundTerminals/list`: one terminal after
/// the turn, none after the command ended. The command's `item/completed`
/// arrives after `turn/completed`.
#[test]
fn background_terminals_are_found_by_polling_after_the_turn() {
    let fx = load("background_terminal");
    let r = replay(&fx).unwrap();
    let polls: Vec<usize> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::BackgroundUpdated { tasks } => Some(tasks.len()),
            _ => None,
        })
        .collect();
    assert_eq!(polls, [1, 0]);
    let methods = out_methods(&fx);
    let turn_done = methods.iter().position(|m| m == "turn/completed").unwrap();
    assert!(
        methods[turn_done..].iter().any(|m| m == "item/completed"),
        "the command item completes after the turn"
    );
}

/// Regression (round 14 review): the command that outlives its turn is not
/// closed as failed at `turn/completed`; it completes once, with the real
/// status, when its `item/completed` arrives after the turn.
#[test]
fn a_background_command_completes_after_the_turn_with_its_real_status() {
    let r = replay(&load("background_terminal")).unwrap();
    let shell = r
        .events
        .iter()
        .find_map(|e| match e {
            CanonEvent::ToolStarted {
                node_id,
                tool_kind: ToolKind::Shell,
                ..
            } => Some(node_id.clone()),
            _ => None,
        })
        .unwrap();
    let completions: Vec<(usize, ToolStatus)> = r
        .events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            CanonEvent::ToolCompleted { node_id, status } if *node_id == shell => {
                Some((i, *status))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        completions.len(),
        1,
        "exactly one completion: {completions:?}"
    );
    let (at, status) = completions[0];
    assert_eq!(status, ToolStatus::Ok);
    let turn_done = r
        .events
        .iter()
        .position(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .unwrap();
    assert!(turn_done < at, "the completion comes after the turn");
    let first_poll = r
        .events
        .iter()
        .position(|e| matches!(e, CanonEvent::BackgroundUpdated { .. }))
        .unwrap();
    assert!(first_poll < at, "the poll saw the command still running");
}

/// The other side of the same rule: a command that is still running when the
/// turn is interrupted closes with the turn (trap 6), exactly once.
#[test]
fn a_command_of_an_interrupted_turn_closes_with_the_turn() {
    let r = replay(&load("interrupt_during_tool")).unwrap();
    let closed: Vec<(usize, ToolStatus)> = r
        .events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            CanonEvent::ToolCompleted { status, .. } => Some((i, *status)),
            _ => None,
        })
        .collect();
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0].1, ToolStatus::Failed);
    let turn_done = r
        .events
        .iter()
        .position(|e| {
            matches!(
                e,
                CanonEvent::TurnCompleted {
                    status: TurnStatus::Interrupted,
                    ..
                }
            )
        })
        .unwrap();
    assert!(closed[0].0 < turn_done);
}

/// Live deltas: `item/agentMessage/delta` arrives before the completed
/// message, and the deltas add up to it.
#[test]
fn live_deltas_arrive_before_the_completed_message() {
    let r = run("simple");
    let first = r
        .events
        .iter()
        .position(|e| matches!(e, CanonEvent::MessageDelta { .. }))
        .unwrap();
    let done = r
        .events
        .iter()
        .position(|e| matches!(e, CanonEvent::MessageCompleted { .. }))
        .unwrap();
    assert!(first < done);
    let joined: String = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::MessageDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(joined, last_message(&r));
}

/// Terminal status quality is strong: every started turn of every recording
/// ends with one `turn/completed` that carries a definite status.
#[test]
fn every_turn_ends_with_one_definite_terminal_status() {
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl"))
        .collect();
    names.sort();
    assert!(names.len() >= 18);
    for file in names {
        let name = file.trim_end_matches(".jsonl");
        let r = run(name);
        let started = r
            .events
            .iter()
            .filter(|e| matches!(e, CanonEvent::TurnStarted { .. }))
            .count();
        let ended: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                CanonEvent::TurnCompleted { status, .. } => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(started, ended.len(), "{name}");
        assert!(
            ended.iter().all(|s| matches!(
                s,
                TurnStatus::Done | TurnStatus::Interrupted | TurnStatus::Failed
            )),
            "{name}: {ended:?}"
        );
    }
}

/// Native ids: thread, turn, and item ids are kept in `provider_refs`; the
/// app ids are ordinals (L6).
#[test]
fn native_ids_are_kept_beside_ordinal_app_ids() {
    let r = run("tool_call_approve");
    let mut seen = Vec::new();
    for e in &r.events {
        match e {
            CanonEvent::SessionStarted { provider_refs, .. } => {
                assert!(provider_refs["thread_id"].is_string());
                seen.push("thread");
            }
            CanonEvent::TurnStarted {
                turn_id,
                provider_refs,
            } => {
                assert!(turn_id.starts_with("turn-"));
                assert!(provider_refs["turn_id"].is_string());
                seen.push("turn");
            }
            CanonEvent::ToolStarted {
                node_id,
                provider_refs,
                ..
            } => {
                assert!(node_id.starts_with("node-"));
                assert!(provider_refs["item_id"].is_string());
                seen.push("item");
            }
            CanonEvent::ApprovalRequested { request_id, .. } => {
                assert!(request_id.starts_with("req-"));
                seen.push("request");
            }
            _ => {}
        }
    }
    for kind in ["thread", "turn", "item", "request"] {
        assert!(seen.contains(&kind), "{kind} missing");
    }
}

/// Usage: `thread/tokenUsage/updated` gives token counts during the turn;
/// Codex reports no cost (unknown, not zero).
#[test]
fn usage_has_tokens_but_no_cost() {
    let r = run("multi_turn");
    let usage: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::UsageUpdated { usage } => Some(usage),
            _ => None,
        })
        .collect();
    assert!(usage.len() >= 2);
    for u in usage {
        assert!(u.input_tokens.unwrap() > 0);
        assert!(u.cost_usd.is_none(), "unknown, not zero");
    }
}
