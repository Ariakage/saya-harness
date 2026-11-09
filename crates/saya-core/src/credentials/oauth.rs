use super::{CredentialRef, CredentialScope, FileCredentialStore};
use rmcp::transport::auth::{
    AuthError, CredentialRefreshGuard, CredentialStore, StoredCredentials,
};

/// Resource-scoped SDK store. Refreshes use a separate cross-process lock from record I/O.
#[derive(Clone)]
pub struct McpCredentialStore {
    pub(super) store: FileCredentialStore,
    pub(super) reference: CredentialRef,
    pub(super) scope: CredentialScope,
}

impl std::fmt::Debug for McpCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpCredentialStore")
            .field("reference", &self.reference)
            .finish()
    }
}
impl McpCredentialStore {
    pub(crate) async fn oauth_resource(&self) -> Result<String, crate::contract::BoundaryError> {
        let resolved = self.store.resolve(self.reference, &self.scope).await?;
        Ok(resolved
            .metadata
            .oauth_resource
            .unwrap_or_else(|| self.scope.audience.clone()))
    }
}

fn auth_error(error: crate::contract::BoundaryError) -> AuthError {
    AuthError::CredentialStoreError(error.0.code)
}

#[async_trait::async_trait]
impl CredentialStore for McpCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let reference = self.reference;
        let scope = self.scope.clone();
        self.store
            .run(move |inner| inner.load_oauth(reference, &scope))
            .await
            .map_err(auth_error)
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let reference = self.reference;
        let scope = self.scope.clone();
        self.store
            .run(move |inner| inner.save_oauth(reference, &scope, credentials))
            .await
            .map_err(auth_error)
    }
    async fn clear(&self) -> Result<(), AuthError> {
        self.store
            .revoke(self.reference, &self.scope)
            .await
            .map_err(auth_error)
    }
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let reference = self.reference;
        self.store
            .run(move |inner| inner.refresh_lock(reference))
            .await
            .map(|guard| Some(CredentialRefreshGuard::new(guard)))
            .map_err(auth_error)
    }
    async fn on_refresh_token_rejected(
        &self,
        credentials: &StoredCredentials,
    ) -> Result<(), AuthError> {
        let reference = self.reference;
        let scope = self.scope.clone();
        let credentials = credentials.clone();
        self.store
            .run(move |inner| inner.reject_oauth(reference, &scope, &credentials))
            .await
            .map_err(auth_error)
    }
}
