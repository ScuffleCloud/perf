use std::net::SocketAddr;

#[derive(Debug, smart_default::SmartDefault, serde::Deserialize)]
#[serde(default)]
pub struct Config {
	#[default = "info"]
	pub level: String,
	#[default(None)]
	pub metrics_bind: Option<SocketAddr>,
}

scuffle_settings::bootstrap!(Config);
