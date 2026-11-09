//! Host-side resolution of explicit MCP credential references. Runs never open a login browser.
pub use saya_core::credentials::{McpOAuthLogin, OAuthLoginOptions};
use saya_core::{
    boundary::{Cancellation, Operation},
    contract::{BoundaryError, ErrorInfo},
    credentials::{CredentialScope, CredentialValue, FileCredentialStore},
    mcp::{
        McpHttpClient,
        config::{HttpMcpSettings, McpHttpAuthorization, McpHttpAuthorizer},
    },
};

#[derive(Clone)]
pub struct StoredMcpAuthorizer {
    store: FileCredentialStore,
}
impl StoredMcpAuthorizer {
    pub fn new(store: FileCredentialStore) -> Self {
        Self { store }
    }
}
fn failure() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "needs_auth".into(),
            message: "MCP credential reference requires authorization".into(),
        },
        None,
        None,
    )
}
impl McpHttpAuthorizer for StoredMcpAuthorizer {
    fn authorize<'a>(
        &'a self,
        server: &'a str,
        settings: &'a HttpMcpSettings,
        client: McpHttpClient,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, McpHttpAuthorization> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(failure());
            }
            let reference = settings.credential_ref.ok_or_else(failure)?;
            let scope =
                CredentialScope::for_mcp(server, &settings.endpoint, settings.scopes.clone())?;
            let resolved = self.store.resolve(reference, &scope).await?;
            match resolved.value {
                CredentialValue::Secret(_) => Ok(McpHttpAuthorization {
                    client: client.with_stored_bearer(self.store.clone(), reference, scope)?,
                    bearer_token: None,
                }),
                CredentialValue::OAuth(_) => {
                    let store = self.store.oauth(reference, scope)?;
                    let client = client.with_oauth_store(&settings.endpoint, store).await?;
                    Ok(McpHttpAuthorization {
                        client,
                        bearer_token: None,
                    })
                }
            }
        })
    }
}
