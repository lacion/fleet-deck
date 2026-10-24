//! Claude usage as per-model running totals (B14, B21; DL-442, DL-443).
//!
//! The CLI reports `result.modelUsage`: the running total of the whole
//! process, per model, with the subagents in it. `result.usage` holds the
//! main thread of one turn only. The core counts the growth of the per-model
//! totals, so a resume, a fork and a subagent are all counted once. A resumed
//! process counts on from the totals that it restored from the transcript.

use std::collections::BTreeMap;

use fleetdeck_proto::event::{USAGE_SCHEME, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::frames::ResultFrame;

/// The running totals of one model in one CLI process
/// (`result.modelUsage.<model>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelTotals {
    /// `inputTokens`.
    pub input: u64,
    /// `outputTokens`.
    pub output: u64,
    /// `cacheReadInputTokens`.
    pub cache_read: u64,
    /// `cacheCreationInputTokens`.
    pub cache_write: u64,
    /// `costUSD`.
    pub cost_usd: f64,
}

impl ModelTotals {
    fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let count = |k: &str| obj.get(k).and_then(Value::as_u64).unwrap_or(0);
        Some(Self {
            input: count("inputTokens"),
            output: count("outputTokens"),
            cache_read: count("cacheReadInputTokens"),
            cache_write: count("cacheCreationInputTokens"),
            cost_usd: obj.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
        })
    }

    /// The growth since `last`. A field under its earlier value means that the
    /// count of this model started again: the whole current value is new.
    fn since(&self, last: Option<&Self>) -> Self {
        let Some(last) = last else {
            return self.clone();
        };
        if self.input < last.input
            || self.output < last.output
            || self.cache_read < last.cache_read
            || self.cache_write < last.cache_write
            || self.cost_usd < last.cost_usd
        {
            return self.clone();
        }
        Self {
            input: self.input - last.input,
            output: self.output - last.output,
            cache_read: self.cache_read - last.cache_read,
            cache_write: self.cache_write - last.cache_write,
            cost_usd: self.cost_usd - last.cost_usd,
        }
    }

    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cost_usd += other.cost_usd;
    }
}

/// The running totals of a process, by model id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessTotals {
    pub models: BTreeMap<String, ModelTotals>,
}

/// What the CLI counts from when the process starts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum UsageBase {
    /// A new conversation: the CLI counts from zero.
    #[default]
    Zero,
    /// A resume or a fork: the totals that the CLI restores (the last
    /// `cost-state` record of the transcript; empty when it has none).
    Restored(ProcessTotals),
    /// A resume or a fork whose transcript the adapter could not read.
    Unknown,
}

/// The usage state of a core; it is part of the checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessUsage {
    /// The per-model totals of the last `result` that had `modelUsage`, or
    /// the base before the first one. `None`: the token base is not known
    /// (an unknown base, an older checkpoint, or the last result had no
    /// `modelUsage`).
    pub last: Option<ProcessTotals>,
    /// The cumulative cost of the process at the last `result` (or the base).
    /// It moves at every result that has a cost, with or without
    /// `modelUsage`. `None`: not known.
    pub cost_cursor: Option<f64>,
}

impl ProcessTotals {
    /// `result.modelUsage`; `None` when the frame has no object there.
    #[must_use]
    pub fn from_model_usage(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        Some(Self {
            models: obj
                .iter()
                .filter_map(|(model, m)| Some((model.clone(), ModelTotals::from_value(m)?)))
                .collect(),
        })
    }

    /// A `cost-state` record of a transcript (`modelUsage`, same keys).
    #[must_use]
    pub fn from_cost_state(v: &Value) -> Option<Self> {
        Self::from_model_usage(v.get("modelUsage")?)
    }

    /// The sum of `cost_usd` over the models.
    #[must_use]
    pub fn cost(&self) -> f64 {
        self.models.values().map(|m| m.cost_usd).sum()
    }

    /// The sum over the models, as one `Usage` (scheme 2, cost not inferred).
    #[must_use]
    pub fn usage(&self) -> Usage {
        let mut sum = ModelTotals::default();
        for m in self.models.values() {
            sum.add(m);
        }
        Usage {
            input_tokens: Some(sum.input),
            output_tokens: Some(sum.output),
            cache_read_tokens: Some(sum.cache_read),
            cache_write_tokens: Some(sum.cache_write),
            cost_usd: Some(sum.cost_usd),
            cost_inferred: false,
            scheme: Some(USAGE_SCHEME),
            ..Usage::default()
        }
    }
}

impl ProcessUsage {
    /// `Zero` gives `{ last: Some(empty), cost_cursor: Some(0.0) }`;
    /// `Restored(t)` gives `{ last: Some(t), cost_cursor: Some(t.cost()) }`;
    /// `Unknown` gives `{ last: None, cost_cursor: None }`.
    #[must_use]
    pub fn new(base: &UsageBase) -> Self {
        match base {
            UsageBase::Zero => Self {
                last: Some(ProcessTotals::default()),
                cost_cursor: Some(0.0),
            },
            UsageBase::Restored(t) => Self {
                last: Some(t.clone()),
                cost_cursor: Some(t.cost()),
            },
            UsageBase::Unknown => Self {
                last: None,
                cost_cursor: None,
            },
        }
    }

    /// The state of a checkpoint written before this design: no token base,
    /// and the cost cursor of its `last_total_cost`.
    #[must_use]
    pub fn legacy(last_total_cost: Option<f64>) -> Self {
        Self {
            last: None,
            cost_cursor: last_total_cost,
        }
    }

    /// The delta of one `result`, and the new state.
    pub fn delta(&mut self, res: &ResultFrame) -> Usage {
        let current = res
            .body
            .get("modelUsage")
            .and_then(ProcessTotals::from_model_usage);
        let now_cost = res
            .total_cost_usd
            .or_else(|| current.as_ref().map(ProcessTotals::cost));
        let mut usage = Usage {
            scheme: Some(USAGE_SCHEME),
            cost_inferred: false,
            ..Usage::default()
        };
        // The cost of the per-model growth (rule 3), when it applies.
        let mut model_cost: Option<f64> = None;
        match (&current, &self.last) {
            (Some(cur), Some(last)) => {
                let mut sum = ModelTotals::default();
                for (model, totals) in &cur.models {
                    sum.add(&totals.since(last.models.get(model)));
                }
                usage.input_tokens = Some(sum.input);
                usage.output_tokens = Some(sum.output);
                usage.cache_read_tokens = Some(sum.cache_read);
                usage.cache_write_tokens = Some(sum.cache_write);
                model_cost = Some(sum.cost_usd);
                self.last = current.clone();
            }
            (Some(_), None) => {
                turn_tokens(res, &mut usage);
                self.last = current.clone();
            }
            (None, _) => {
                tracing::debug!("claude result without modelUsage");
                turn_tokens(res, &mut usage);
                self.last = None;
            }
        }
        match (now_cost, self.cost_cursor) {
            (Some(n), Some(c)) => {
                usage.cost_usd = Some(match model_cost {
                    Some(m) => m,
                    None if n >= c => n - c,
                    None => n,
                });
            }
            _ => usage.cost_partial = true,
        }
        if now_cost.is_some() {
            self.cost_cursor = now_cost;
        }
        usage
    }
}

/// The tokens of this turn from `result.usage` (the main thread only), marked
/// as a lower bound.
fn turn_tokens(res: &ResultFrame, usage: &mut Usage) {
    let u = res.usage.as_ref();
    let tokens = |k: &str| u.and_then(|u| u.get(k)).and_then(Value::as_u64);
    usage.input_tokens = tokens("input_tokens");
    usage.output_tokens = tokens("output_tokens");
    usage.cache_read_tokens = tokens("cache_read_input_tokens");
    usage.cache_write_tokens = tokens("cache_creation_input_tokens");
    usage.tokens_partial = true;
}
