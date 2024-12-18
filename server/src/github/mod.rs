use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use installation::InstallationClient;
use octocrab::models::{AppId, Installation, InstallationId, RepositoryId, UserId};
use octocrab::Octocrab;

pub mod auto_start;
pub mod config;
pub mod installation;
pub mod webhook;

pub struct GitHubService {
    client: Octocrab,
    installations: parking_lot::Mutex<HashMap<InstallationId, Arc<InstallationClient>>>,
}

impl GitHubService {
    pub async fn new(app_id: AppId, key: jsonwebtoken::EncodingKey) -> anyhow::Result<Self> {
        let client = Octocrab::builder()
            .app(app_id, key)
            .build()
            .context("build octocrab client")?;

        let mut installations = HashMap::new();
        let mut user_to_installation = HashMap::new();

        for installation in client.apps().installations().send().await.context("get installations")? {
            let client = client.installation(installation.id).context("build installation client")?;
            let login = installation.account.login.clone();
            let installation_id = installation.id;
            let account_id = installation.account.id;

            let client = Arc::new(
                InstallationClient::new(client, installation)
                    .await
                    .with_context(|| format!("initialize installation client for {}", login))?,
            );

            client.fetch_repositories().await?;

            user_to_installation.insert(account_id, installation_id);
            installations.insert(installation_id, client);
        }

        Ok(Self {
            client,
            installations: parking_lot::Mutex::new(installations),
        })
    }

    pub fn get_client(&self, installation_id: InstallationId) -> Option<Arc<InstallationClient>> {
        self.installations.lock().get(&installation_id).cloned()
    }

    pub fn get_client_by_user(&self, user_id: UserId) -> Option<Arc<InstallationClient>> {
        self.installations
            .lock()
            .values()
            .find(|client| client.installation().account.id == user_id)
            .cloned()
    }

    pub fn get_client_by_repo(&self, repo_id: RepositoryId) -> Option<Arc<InstallationClient>> {
        self.installations
            .lock()
            .values()
            .find(|client| client.has_repository(repo_id))
            .cloned()
    }

    pub fn installations(&self) -> HashMap<InstallationId, Arc<InstallationClient>> {
        self.installations.lock().clone()
    }

    pub async fn update_installation(&self, installation: Installation) -> anyhow::Result<()> {
        let install = self.installations.lock().get(&installation.id).cloned();
        if let Some(install) = install {
            install.update_installation(installation);
            install.fetch_repositories().await?;
        } else {
            let installation_id = installation.id;
            let login = installation.account.login.clone();
            let client = self
                .client
                .installation(installation_id)
                .context("build installation client")?;

            let client = Arc::new(
                InstallationClient::new(client, installation)
                    .await
                    .with_context(|| format!("initialize installation client for {}", login))?,
            );

            client.fetch_repositories().await?;

            self.installations.lock().insert(installation_id, client);
        }

        Ok(())
    }

    pub fn delete_installation(&self, installation_id: InstallationId) {
        self.installations.lock().remove(&installation_id);
    }
}

pub use auto_start::{AutoStartConfig, AutoStartSvc};
pub use webhook::{WebhookConfig, WebhookSvc};
