//! The hub-forwarded identity headers.
//!
//! The hub's reverse proxy strips these from inbound requests and re-adds them
//! from the session, so what arrives here is the hub's word on who is calling:
//!
//!   X-Leo-User-Id   — the authenticated user's id (always present)
//!   x-leo-is-admin  — "1" for an admin, "0" otherwise
//!
//! A missing admin header means **not** admin, never unknown-therefore-yes.
//! Everything that changes a host or a guest requires an explicit "1":
//! defining a VM, handing it a GPU, and editing how the box boots are all
//! administration rather than user actions.

use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};
use axum::response::{IntoResponse, Response};

#[derive(Debug, Clone)]
pub struct Caller {
    #[allow(dead_code)]
    pub user_id: String,
    #[allow(dead_code)]
    pub is_admin: bool,
}

/// A caller who is also an admin. Extracting this type rejects with 403 when
/// `x-leo-is-admin` is absent or anything other than "1".
#[derive(Debug, Clone)]
pub struct AdminCaller {
    #[allow(dead_code)]
    pub user_id: String,
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let user_id = parts
            .headers
            .get("x-leo-user-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or(Rejection::Unauthenticated)?;
        // "1" is the only truthy value. Anything else — including "true" and
        // "yes" — is not admin, because a header this process cannot verify
        // should be read in the narrowest way that is still correct.
        let is_admin = parts
            .headers
            .get("x-leo-is-admin")
            .and_then(|v| v.to_str().ok())
            == Some("1");
        Ok(Caller { user_id, is_admin })
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AdminCaller {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let c = Caller::from_request_parts(parts, state).await?;
        if !c.is_admin {
            return Err(Rejection::Forbidden);
        }
        Ok(AdminCaller { user_id: c.user_id })
    }
}

#[derive(Debug)]
pub enum Rejection {
    /// `X-Leo-User-Id` was absent — the request did not come through the hub's
    /// auth layer.
    Unauthenticated,
    Forbidden,
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthenticated => {
                (StatusCode::UNAUTHORIZED, "not authenticated").into_response()
            }
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "changing a host or a space requires an admin",
            )
                .into_response(),
        }
    }
}
