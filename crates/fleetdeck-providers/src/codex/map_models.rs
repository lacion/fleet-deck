//! The `model/list` request and its pages → `session.models` (DL-151).

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ModelChoice;
use serde_json::{Value, json};

use super::super::core::{CodexCore, HostRequest, Output};
use super::str_at;

/// Pages of `model/list` that follow the first one (DL-151).
const MODEL_LIST_MORE_PAGES: u8 = 4;

impl CodexCore {
    /// The `model/list` request for the next page (DL-151).
    pub(super) fn model_list(
        &mut self,
        pages: u8,
        models: Vec<ModelChoice>,
        cursor: Option<&str>,
    ) -> Output {
        let id = self.host_id(HostRequest::ModelList { pages, models });
        let params = cursor.map_or_else(|| json!({}), |c| json!({ "cursor": c }));
        Output::Write(super::super::rpc::request(&id, "model/list", params))
    }

    /// A page of `model/list`: the next page, or `session.models` when the
    /// last page (or the page limit) is reached. No models: no event.
    pub(super) fn on_model_list(
        &mut self,
        pages: u8,
        mut models: Vec<ModelChoice>,
        r: &Value,
    ) -> Vec<Output> {
        models.extend(
            r.get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|m| m.get("hidden").and_then(Value::as_bool) != Some(true))
                .filter_map(|m| {
                    let value = str_at(m, "model").or_else(|| str_at(m, "id"))?;
                    Some(ModelChoice {
                        resolved: None,
                        efforts: super::super::effort_set::model_efforts(m),
                        default_effort: super::super::effort_set::model_default_effort(m),
                        value: value.to_owned(),
                        label: str_at(m, "displayName").unwrap_or(value).to_owned(),
                        description: str_at(m, "description").map(str::to_owned),
                    })
                }),
        );
        if let Some(cursor) = str_at(r, "nextCursor")
            && pages < MODEL_LIST_MORE_PAGES
        {
            return vec![self.model_list(pages + 1, models, Some(cursor))];
        }
        self.model_choices.clone_from(&models);
        if models.is_empty() {
            return Vec::new();
        }
        vec![Output::Emit(Event::SessionModels { models })]
    }
}
