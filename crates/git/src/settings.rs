//! Minimal user settings construction for jj-lib, mirroring the original
//! jjlab's approach: a stacked config with only `[user]` populated.

use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::settings::UserSettings;

pub fn user_settings(name: &str, email: &str) -> std::result::Result<UserSettings, String> {
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