#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
// disable coverage for now.
#![cfg_attr(all(coverage_nightly, test), coverage(off))]

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use scuffle_bootstrap_telemetry::opentelemetry;
use scuffle_bootstrap_telemetry::opentelemetry_sdk::metrics::SdkMeterProvider;
use scuffle_bootstrap_telemetry::opentelemetry_sdk::Resource;
use scuffle_bootstrap_telemetry::prometheus_client::registry::Registry;
use scuffle_metrics::opentelemetry::KeyValue;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

#[derive(Debug, smart_default::SmartDefault, serde::Deserialize)]
#[serde(default)]
pub struct Config {
	#[default = "info"]
	pub level: String,
	#[default(None)]
	pub metrics_bind: Option<SocketAddr>,
	#[default(env_or_default("DATABASE_URL", None))]
	pub db_url: Option<String>,
	pub github: GitHub,
}

#[derive(Debug, smart_default::SmartDefault, serde::Deserialize)]
#[serde(default)]
pub struct GitHub {
	#[default(env_or_default("GITHUB_ACCESS_TOKEN", None))]
	pub access_token: Option<String>,
	#[default(env_or_default("GITHUB_APP_CLIENT_ID", None))]
	pub app_client_id: Option<String>,
	#[default(env_or_default("GITHUB_APP_CLIENT_SECRET", None))]
	pub app_client_secret: Option<String>,
}

fn env_or_default<T: From<String>>(key: &'static str, default: impl Into<T>) -> T {
	std::env::var(key).map(Into::into).unwrap_or_else(|_| default.into())
}

scuffle_settings::bootstrap!(Config);

pub struct Global {
	config: Config,
	metrics_registry: Registry,
	database: sea_orm::DatabaseConnection,
}

impl scuffle_bootstrap::Global for Global {
	type Config = Config;

	async fn init(config: Self::Config) -> anyhow::Result<Arc<Self>> {
		let mut metrics_registry = Registry::default();
		let exporter = scuffle_metrics::prometheus::exporter().build();
		metrics_registry.register_collector(exporter.collector());

		opentelemetry::global::set_meter_provider(
			SdkMeterProvider::builder()
				.with_resource(Resource::new(vec![
					KeyValue::new("service.name", env!("CARGO_PKG_NAME")),
					KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
				]))
				.with_reader(exporter)
				.build(),
		);

		tracing_subscriber::registry()
			.with(
				tracing_subscriber::fmt::layer()
					.with_file(true)
					.with_line_number(true)
					.with_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(config.level.parse()?)),
			)
			.init();

		tracing::info!("starting server.");

		let Some(db_url) = config.db_url.as_deref() else {
			anyhow::bail!("DATABASE_URL is not set");
		};

		let database = sea_orm::Database::connect(db_url).await.context("connect to database")?;

		Ok(Arc::new(Self {
			config,
			metrics_registry,
			database,
		}))
	}
}

impl scuffle_signal::SignalConfig for Global {
	async fn on_shutdown(self: &Arc<Self>) -> anyhow::Result<()> {
		tracing::info!("shutting down server.");
		Ok(())
	}
}

impl scuffle_bootstrap_telemetry::TelemetryConfig for Global {
	async fn health_check(&self) -> Result<(), anyhow::Error> {
		self.database.ping().await.context("ping database")?;
		Ok(())
	}

	fn bind_address(&self) -> Option<std::net::SocketAddr> {
		self.config.metrics_bind
	}

	fn prometheus_metrics_registry(&self) -> Option<&Registry> {
		Some(&self.metrics_registry)
	}
}

scuffle_bootstrap::main! {
	Global {
		scuffle_signal::SignalSvc,
		scuffle_bootstrap_telemetry::TelemetrySvc,
	}
}
