use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::{Json, RequestExt};
use hmac::{Hmac, Mac};
use octocrab::models::webhook_events::EventInstallation;
use scuffle_context::ContextFutExt;
use scuffle_http::backend::HttpServer;
use serde::Serialize;
use sha2::Sha256;

use super::GitHubService;

pub trait WebhookConfig: Send + Sync + 'static {
	fn webhook_secret(&self) -> &str;

	fn bind_address(&self) -> Option<SocketAddr>;

	fn github_service(&self) -> &GitHubService;
}

fn router<C: WebhookConfig>(global: Arc<C>) -> axum::Router {
	axum::Router::new()
		.route("/github/webhook", axum::routing::post(handle_webhook::<C>))
		.with_state(global)
}

#[derive(Debug, Serialize)]
struct Response {
	success: bool,
	message: String,
}

fn verify_gh_signature(headers: &HeaderMap<HeaderValue>, body: &[u8], secret: &str) -> bool {
	let Some(signature) = headers.get("x-hub-signature-256").map(|v| v.as_bytes()) else {
		return false;
	};
	let Some(signature) = signature.get(b"sha256=".len()..).and_then(|v| hex::decode(v).ok()) else {
		return false;
	};

	let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("Cannot create HMAC key");
	mac.update(body);
	mac.verify_slice(&signature).is_ok()
}

async fn handle_webhook<C: WebhookConfig>(
	State(global): State<Arc<C>>,
	request: axum::http::Request<axum::body::Body>,
) -> (StatusCode, Json<Response>) {
	let (parts, body) = request.with_limited_body().into_parts();
	let Some(header) = parts.headers.get("X-GitHub-Event").and_then(|v| v.to_str().ok()) else {
		return (
			StatusCode::BAD_REQUEST,
			Json(Response {
				success: false,
				message: "Missing X-GitHub-Event header".to_string(),
			}),
		);
	};

	let Ok(body) = axum::body::to_bytes(body, 1024 * 1024 * 10).await else {
		return (
			StatusCode::BAD_REQUEST,
			Json(Response {
				success: false,
				message: "Failed to read body".to_string(),
			}),
		);
	};

	if !verify_gh_signature(&parts.headers, &body, global.webhook_secret()) {
		return (
			StatusCode::UNAUTHORIZED,
			Json(Response {
				success: false,
				message: "Invalid signature".to_string(),
			}),
		);
	}

	let event = match octocrab::models::webhook_events::WebhookEvent::try_from_header_and_body(header, &body) {
		Ok(event) => event,
		Err(e) => {
			tracing::error!("Failed to parse event: {:#}", e);
			return (
				StatusCode::INTERNAL_SERVER_ERROR,
				Json(Response {
					success: false,
					message: "Failed to parse event".to_string(),
				}),
			);
		}
	};

	if let Err(err) = handle_event(global, event).await {
		tracing::error!("Failed to handle event: {:#}", err);
		return (
			StatusCode::INTERNAL_SERVER_ERROR,
			Json(Response {
				success: false,
				message: "Failed to handle event".to_string(),
			}),
		);
	}

	(
		StatusCode::OK,
		Json(Response {
			success: true,
			message: "Event handled successfully".to_string(),
		}),
	)
}

pub struct WebhookSvc;

impl<G> scuffle_bootstrap::Service<G> for WebhookSvc
where
	G: WebhookConfig,
{
	async fn enabled(&self, global: &Arc<G>) -> anyhow::Result<bool> {
		Ok(global.bind_address().is_some())
	}

	async fn run(self, global: Arc<G>, ctx: scuffle_context::Context) -> anyhow::Result<()> {
		let bind = global.bind_address().context("missing bind address")?;

		let server = scuffle_http::backend::tcp::TcpServerConfig::builder()
			.with_bind(bind)
			.build()
			.into_server();

		server
			.start(scuffle_http::svc::axum_service(router(global)), 1)
			.await
			.context("start")?;

		tracing::info!("webhook server started on {}", server.local_addr().context("local address")?);

		server.wait().with_context(&ctx).await.transpose().context("wait")?;

		tracing::info!("shutting down webhook server");

		server.shutdown().await.context("shutdown")?;

		tracing::info!("webhook server shutdown");

		Ok(())
	}
}

async fn handle_event<C: WebhookConfig>(
	global: Arc<C>,
	event: octocrab::models::webhook_events::WebhookEvent,
) -> anyhow::Result<()> {
	let installation_id = match event.installation {
		Some(EventInstallation::Full(installation)) => {
			let installation_id = installation.id;
			global
				.github_service()
				.update_installation(*installation)
				.await
				.context("update_installation")?;
			installation_id
		}
		Some(EventInstallation::Minimal(installation)) => installation.id,
		None => {
			tracing::warn!("event does not have installation: {:?}", event.kind);
			return Ok(());
		}
	};

	let Some(client) = global.github_service().get_client(installation_id) else {
		tracing::error!("no client for installation {}", installation_id);
		return Ok(());
	};

	match event.specific {
		octocrab::models::webhook_events::WebhookEventPayload::Installation(event) => {
			dbg!(&event);
		}
		octocrab::models::webhook_events::WebhookEventPayload::InstallationRepositories(event) => {
			for repo in event.repositories_added {
				client.fetch_repository(repo.id).await.context("fetch_repository")?;
			}

			for repo in event.repositories_removed {
				client.remove_repository(repo.id);
			}
		}
		_ => {}
	}

	Ok(())
}
