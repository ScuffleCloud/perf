#![cfg_attr(all(coverage_nightly, test), coverage(off))]

use std::sync::Arc;

use scuffle_bootstrap_telemetry::opentelemetry;
use scuffle_bootstrap_telemetry::opentelemetry_sdk::metrics::SdkMeterProvider;
use scuffle_bootstrap_telemetry::opentelemetry_sdk::Resource;
use scuffle_bootstrap_telemetry::prometheus_client::registry::Registry;
use scuffle_metrics::opentelemetry::KeyValue;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::config::Config;

pub struct Global {
	pub config: Config,
	pub metrics_registry: Registry,
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
					KeyValue::new("service.name", env!("CARGO_BIN_NAME")),
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

		Ok(Arc::new(Self {
			config,
			metrics_registry,
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
		Ok(())
	}

	fn bind_address(&self) -> Option<std::net::SocketAddr> {
		self.config.metrics_bind
	}

	fn prometheus_metrics_registry(&self) -> Option<&Registry> {
		Some(&self.metrics_registry)
	}
}
