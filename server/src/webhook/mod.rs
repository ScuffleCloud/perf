use std::net::SocketAddr;
use std::ops::DerefMut;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::{Json, RequestExt};
use diesel_async::AsyncPgConnection;
use hmac::{Hmac, Mac};
use octocrab::models::webhook_events::payload::{
    InstallationWebhookEventAction, IssueCommentWebhookEventAction, PullRequestWebhookEventAction,
};
use octocrab::models::webhook_events::{EventInstallation, WebhookEventPayload, WebhookEventType};
use octocrab::models::{Installation, InstallationId};
use scuffle_context::ContextFutExt;
use scuffle_http::backend::HttpServer;
use serde::Serialize;
use sha2::Sha256;

pub mod check_event;

use crate::command::{BrawlCommand, BrawlCommandContext, PullRequestCommand};
use crate::github::installation::{GitHubInstallationClient, GitHubRepoClient};

pub trait WebhookConfig: Send + Sync + 'static {
    type InstallationClient: GitHubInstallationClient;

    fn webhook_secret(&self) -> &str;

    fn bind_address(&self) -> Option<SocketAddr>;

    fn installation_client(&self, installation_id: InstallationId) -> Option<Self::InstallationClient>;

    fn update_installation(
        &self,
        installation: Installation,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn delete_installation(&self, installation_id: InstallationId) -> anyhow::Result<()>;

    fn database(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<impl DerefMut<Target = AsyncPgConnection> + Send>> + Send;

    fn uptime(&self) -> std::time::Duration;
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

    let event = match parse_event(header, &body) {
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

#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub sender: Option<octocrab::models::Author>,
    pub repository: Option<octocrab::models::Repository>,
    pub organization: Option<octocrab::models::orgs::Organization>,
    pub installation: Option<octocrab::models::webhook_events::EventInstallation>,
    pub kind: WebhookEventType,
    pub specific: WebhookEventPayload,
}

fn parse_event(header: &str, body: &[u8]) -> anyhow::Result<WebhookEvent> {
    // NOTE: this is inefficient code to simply reuse the code from "derived"
    // serde::Deserialize instead of writing specific deserialization code for the
    // enum.
    let kind = if header.starts_with('"') {
        serde_json::from_str::<WebhookEventType>(header)?
    } else {
        serde_json::from_str::<WebhookEventType>(&format!("\"{header}\""))?
    };

    // Intermediate structure allows to separate the common fields from
    // the event specific one.
    #[derive(serde::Deserialize)]
    struct Intermediate {
        sender: Option<octocrab::models::Author>,
        repository: Option<octocrab::models::Repository>,
        organization: Option<octocrab::models::orgs::Organization>,
        installation: Option<octocrab::models::webhook_events::EventInstallation>,
        #[serde(flatten)]
        specific: serde_json::Value,
    }

    let Intermediate {
        sender,
        repository,
        organization,
        installation,
        mut specific,
    } = serde_json::from_slice::<Intermediate>(body)?;

    // Bug: OctoCrab wrongly requires the pusher to have an email
    // Remove when https://github.com/XAMPPRocky/octocrab/issues/486 is fixed
    if kind == WebhookEventType::Push {
        if let Some(pusher) = specific.get_mut("pusher") {
            if let Some(email) = pusher.get_mut("email") {
                if email.is_null() {
                    *email = serde_json::Value::String("".to_owned())
                }
            }
        }
    }

    let specific = kind.parse_specific_payload(specific)?;

    Ok(WebhookEvent {
        sender,
        repository,
        organization,
        installation,
        kind,
        specific,
    })
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

async fn handle_event<C: WebhookConfig>(global: Arc<C>, mut event: WebhookEvent) -> anyhow::Result<()> {
    let installation_id = match &event.installation {
        Some(EventInstallation::Full(installation)) => {
            let installation_id = installation.id;

            global
                .update_installation(*installation.clone())
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

    let Some(client) = global.installation_client(installation_id) else {
        tracing::error!("no client for installation {}", installation_id);
        return Ok(());
    };

    match event.specific {
        WebhookEventPayload::Installation(install_event) => match install_event.action {
            InstallationWebhookEventAction::Deleted | InstallationWebhookEventAction::Suspend => {
                global.delete_installation(installation_id).context("delete_installation")?;
            }
            _ => {}
        },
        WebhookEventPayload::InstallationRepositories(event) => {
            for repo in event.repositories_added {
                client.fetch_repository(repo.id).await.context("fetch_repository")?;
            }

            for repo in event.repositories_removed {
                client.remove_repository(repo.id);
            }
        }
        WebhookEventPayload::Repository(_) => {
            if let Some(repo) = event.repository {
                client.set_repository(repo).await.context("set repository")?;
            }
        }
        WebhookEventPayload::PullRequest(mut pull_request_event) => {
            let command = match pull_request_event.action {
                PullRequestWebhookEventAction::Opened => BrawlCommand::PullRequest(PullRequestCommand::Opened),
                PullRequestWebhookEventAction::Synchronize => BrawlCommand::PullRequest(PullRequestCommand::Push),
                PullRequestWebhookEventAction::ConvertedToDraft => BrawlCommand::PullRequest(PullRequestCommand::IntoDraft),
                PullRequestWebhookEventAction::ReadyForReview => {
                    BrawlCommand::PullRequest(PullRequestCommand::ReadyForReview)
                }
                PullRequestWebhookEventAction::Closed => BrawlCommand::PullRequest(PullRequestCommand::Closed),
                _ => return Ok(()),
            };

            let Some(repo_id) = event
                .repository
                .as_ref()
                .or(pull_request_event.pull_request.repo.as_deref())
                .map(|r| r.id)
            else {
                return Ok(());
            };

            let Some(repo_client) = client.get_repository(repo_id) else {
                return Ok(());
            };

            repo_client.set_pull_request(pull_request_event.pull_request.clone()).await;

            let pr = repo_client
                .get_pull_request(pull_request_event.pull_request.number)
                .await
                .context("get_pull_request")?;

            let Some(author) = event
                .sender
                .take()
                .or_else(|| pull_request_event.pull_request.user.take().map(|u| *u))
            else {
                return Ok(());
            };

            command
                .handle(
                    global.database().await?.deref_mut(),
                    BrawlCommandContext {
                        repo: &repo_client,
                        user: author.into(),
                        pr,
                    },
                )
                .await?;
        }
        WebhookEventPayload::IssueComment(issue_comment_event)
            if issue_comment_event.action == IssueCommentWebhookEventAction::Created
                && issue_comment_event.issue.pull_request.is_some() =>
        {
            let Some(body) = issue_comment_event.comment.body.as_ref() else {
                return Ok(());
            };

            let Ok(command) = BrawlCommand::from_str(body) else {
                return Ok(());
            };

            let Some(repo) = event.repository else {
                return Ok(());
            };

            let Some(repo_client) = client.get_repository(repo.id) else {
                return Ok(());
            };

            let pr = repo_client.get_pull_request(issue_comment_event.issue.number).await?;

            command
                .handle(
                    global.database().await?.deref_mut(),
                    BrawlCommandContext {
                        repo: &repo_client,
                        user: issue_comment_event.comment.user.into(),
                        pr,
                    },
                )
                .await?;
        }
        WebhookEventPayload::CheckRun(check_run_event) => {
            let repo = event.repository.context("missing repository")?;
            let Some(repo_client) = client.get_repository(repo.id) else {
                return Ok(());
            };

            check_event::handle(&repo_client, global.database().await?.deref_mut(), check_run_event.check_run)
                .await
                .context("handle_check_event")?;
        }
        _ => {}
    }

    Ok(())
}
