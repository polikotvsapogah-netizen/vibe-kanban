use std::sync::Arc;

use api_types::ProfileResponse;
use tokio::sync::{Mutex as TokioMutex, OwnedMutexGuard, RwLock};

use super::oauth_credentials::{Credentials, OAuthCredentials};

#[derive(Clone)]
pub struct AuthContext {
    oauth: Arc<OAuthCredentials>,
    profile: Arc<RwLock<Option<ProfileResponse>>>,
    remote_auth_degraded_slug: Arc<RwLock<Option<String>>>,
    refresh_lock: Arc<TokioMutex<()>>,
}

impl AuthContext {
    pub fn new(
        oauth: Arc<OAuthCredentials>,
        profile: Arc<RwLock<Option<ProfileResponse>>>,
    ) -> Self {
        Self {
            oauth,
            profile,
            remote_auth_degraded_slug: Arc::new(RwLock::new(None)),
            refresh_lock: Arc::new(TokioMutex::new(())),
        }
    }

    pub async fn get_credentials(&self) -> Option<Credentials> {
        self.oauth.get().await
    }

    pub async fn ensure_credentials_loaded(&self) -> std::io::Result<Option<Credentials>> {
        if let Some(creds) = self.oauth.get().await {
            return Ok(Some(creds));
        }

        self.oauth.load().await?;
        Ok(self.oauth.get().await)
    }

    pub async fn save_credentials(&self, creds: &Credentials) -> std::io::Result<()> {
        self.oauth.save(creds).await
    }

    pub async fn clear_credentials(&self) -> std::io::Result<()> {
        self.oauth.clear_persisted().await
    }

    pub async fn clear_session_credentials(&self) -> std::io::Result<()> {
        self.oauth.clear_session().await
    }

    pub async fn remote_auth_degraded_slug(&self) -> Option<String> {
        self.remote_auth_degraded_slug.read().await.clone()
    }

    pub async fn set_remote_auth_degraded_slug(&self, slug: impl Into<String>) {
        *self.remote_auth_degraded_slug.write().await = Some(slug.into());
    }

    pub async fn clear_remote_auth_degraded_slug(&self) {
        *self.remote_auth_degraded_slug.write().await = None;
    }

    pub async fn cached_profile(&self) -> Option<ProfileResponse> {
        self.profile.read().await.clone()
    }

    pub async fn set_profile(&self, profile: ProfileResponse) {
        *self.profile.write().await = Some(profile)
    }

    pub async fn clear_profile(&self) {
        *self.profile.write().await = None
    }

    pub async fn refresh_guard(&self) -> OwnedMutexGuard<()> {
        self.refresh_lock.clone().lock_owned().await
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn test_credentials() -> Credentials {
        Credentials {
            access_token: Some("access-token".to_string()),
            refresh_token: "refresh-token".to_string(),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn ensure_credentials_loaded_restores_persisted_refresh_token() {
        let temp_dir = TempDir::new().unwrap();
        let oauth = Arc::new(OAuthCredentials::new(
            temp_dir.path().join("credentials.json"),
        ));
        let auth = AuthContext::new(oauth.clone(), Arc::new(RwLock::new(None)));

        auth.save_credentials(&test_credentials()).await.unwrap();
        auth.clear_session_credentials().await.unwrap();

        let loaded = auth.ensure_credentials_loaded().await.unwrap();

        assert!(loaded.is_some(), "persisted credentials should be restored");
        assert_eq!(
            loaded.unwrap().refresh_token,
            "refresh-token",
            "restored credentials should come from disk"
        );
        assert!(
            auth.get_credentials().await.is_some(),
            "restored credentials should be cached in memory"
        );
    }

    #[tokio::test]
    async fn ensure_credentials_loaded_stays_empty_without_file() {
        let temp_dir = TempDir::new().unwrap();
        let oauth = Arc::new(OAuthCredentials::new(
            temp_dir.path().join("credentials.json"),
        ));
        let auth = AuthContext::new(oauth, Arc::new(RwLock::new(None)));

        let loaded = auth.ensure_credentials_loaded().await.unwrap();

        assert!(
            loaded.is_none(),
            "missing credentials file should stay logged out"
        );
    }
}
