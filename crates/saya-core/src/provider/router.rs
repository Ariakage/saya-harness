//! Route execution and independent review requests to explicitly configured profiles.
use super::failure;
use crate::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, Operation},
    contract::{BoundaryError, ModelPurpose, Session},
};
use std::{collections::BTreeMap, sync::Arc};

/// Immutable profile registry for one execution session. Review may select a
/// different registered profile; execution cannot switch its profile mid-run.
pub struct ProfileRouter {
    execution_profile: String,
    profiles: BTreeMap<String, Arc<dyn Model>>,
}
impl ProfileRouter {
    /// Construct a registry with an existing execution profile. Missing or
    /// duplicate profiles must be rejected by the caller while building the map.
    pub fn new(
        execution_profile: String,
        profiles: BTreeMap<String, Arc<dyn Model>>,
    ) -> Result<Self, BoundaryError> {
        if execution_profile.is_empty()
            || !profiles.contains_key(&execution_profile)
            || profiles.keys().any(String::is_empty)
        {
            return Err(failure(
                "invalid_config",
                "execution profile is missing or invalid",
            ));
        }
        Ok(Self {
            execution_profile,
            profiles,
        })
    }
}
impl Model for ProfileRouter {
    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        if session.profile != self.execution_profile {
            return Err(failure(
                "invalid_config",
                "execution profile differs from the session",
            ));
        }
        self.profiles
            .get(&self.execution_profile)
            .ok_or_else(|| failure("invalid_config", "execution profile is unavailable"))?
            .validate_session(session)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if request.purpose == ModelPurpose::Execution
                && request.profile != self.execution_profile
            {
                return Err(failure(
                    "invalid_config",
                    "execution profile cannot change during a session",
                ));
            }
            if request.purpose == ModelPurpose::Review && !request.tools.is_empty() {
                return Err(failure(
                    "invalid_config",
                    "review requests cannot contain tools",
                ));
            }
            let model = self.profiles.get(&request.profile).ok_or_else(|| {
                failure("invalid_config", "requested model profile is unavailable")
            })?;
            model.start(request, cancel).await
        })
    }
}
