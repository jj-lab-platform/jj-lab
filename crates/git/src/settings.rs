//! Minimal user settings construction for jj-lib, mirroring the original
//! jjlab's approach: a stacked config with only `[user]` populated.

use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::settings::UserSettings;

/// Default author identity for server-initiated commits, overridable via
/// `JJLAB_AUTHOR_NAME` / `JJLAB_AUTHOR_EMAIL` (falls back to jj-lab).
pub const DEFAULT_AUTHOR: (&str, &str) = ("jj-lab", "jj-lab@dev");

/// Resolve the server author identity from the environment.
pub fn author_identity() -> (String, String) {
    let name = std::env::var("JJLAB_AUTHOR_NAME")
        .unwrap_or_else(|_| DEFAULT_AUTHOR.0.to_string());
    let email = std::env::var("JJLAB_AUTHOR_EMAIL")
        .unwrap_or_else(|_| DEFAULT_AUTHOR.1.to_string());
    (name, email)
}

/// Build `UserSettings` from the currently-configured server author identity.
pub fn user_settings() -> std::result::Result<UserSettings, String> {
    let (name, email) = author_identity();
    user_settings_named(&name, &email)
}

fn user_settings_named(name: &str, email: &str) -> std::result::Result<UserSettings, String> {
    let mut config = StackedConfig::with_defaults();
    let user_text = format!(
        "[user]\nname = \"{}\"\nemail = \"{}\"\n",
        name.replace('\\', "\\\\").replace('"', "\\\""),
        email.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let layer = ConfigLayer::parse(ConfigSource::User, &user_text)
        .map_err(|e| format!("parse user config: {e}"))?;
    config.add_layer(layer);
    UserSettings::from_config(config).map_err(|e| format!("build settings: {e}"))
}