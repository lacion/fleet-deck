//! R13 P7b §3.2: the settings `allow_desktop_click` and
//! `allow_desktop_input_any`: the save protocol and the elevation rule.

use serde_json::json;

use super::{Fixture, change_with, ok_prepare};
use crate::settings::SaveError;

#[test]
fn turning_agent_clicks_or_broader_input_on_needs_elevation_and_off_does_not() {
    for key in ["allow_desktop_click", "allow_desktop_input_any"] {
        let fx = Fixture::new(Some("name = \"box\"\n"));
        let w = fx.writer();
        let view = crate::settings::view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
        assert_eq!(view.values[key], json!(false));
        let on = change_with(&view.revision, json!({ key: true }));
        let err = w.save(&on, false, ok_prepare).unwrap_err();
        assert_eq!(
            err,
            SaveError::ElevationRequired {
                keys: vec![key.into()]
            }
        );
        // The refused save wrote nothing.
        assert!(!fx.text().contains(key), "{}", fx.text());
        let (saved, new, ()) = w.save(&on, true, ok_prepare).unwrap();
        assert!(new.allow_desktop_click || new.allow_desktop_input_any);
        assert_eq!(saved.applied, vec![key]);
        let view = crate::settings::view_station(&fx.config, &fx.env, &fx.store, &[]).unwrap();
        assert_eq!(view.values[key], json!(true));
        let off = change_with(&view.revision, json!({ key: false }));
        let (saved, new, ()) = w.save(&off, false, ok_prepare).unwrap();
        assert!(!new.allow_desktop_click && !new.allow_desktop_input_any);
        assert_eq!(saved.applied, vec![key]);
    }
}
