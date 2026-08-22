//! Browser sessions, so a person never types a token.
//!
//! Two very different clients hit this API. The ESP32 carries a static bearer
//! token compiled into firmware — it cannot do OAuth and has nowhere to keep a
//! cookie. A browser can hold an `HttpOnly` cookie but should never hold the
//! device token, because anything in page JavaScript is public.
//!
//! So: the Spotify authorization a person already has to complete once doubles
//! as the browser login. Finishing it mints a session, and the cookie carries
//! only an opaque handle — the handle maps to *which account* signed in, not
//! merely "signed in or not," so two different people's cookies resolve to two
//! different accounts. Sessions are persisted beside the token store so a
//! redeploy does not sign anyone out.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Duration, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{account::AccountId, error::AppError};

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "muse_session";
/// How long a browser stays signed in. Long, because this is a shelf device in
/// someone's home, not a bank.
const SESSION_DAYS: i64 = 365;
/// Ceiling on stored sessions, so repeated logins cannot grow the file forever.
const MAX_SESSIONS: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    account_id: AccountId,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredSessions {
    /// Session handle to the account it signs in and when that expires.
    sessions: HashMap<String, SessionRecord>,
}

/// Issues and checks browser sessions.
pub struct SessionStore {
    path: PathBuf,
    sessions: RwLock<HashMap<String, SessionRecord>>,
}

impl SessionStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Load persisted sessions. A missing or unreadable file is not an error:
    /// it only means everyone signs in again.
    pub async fn load(&self) -> Result<usize, AppError> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                tracing::warn!(%error, "could not read session store");
                return Ok(0);
            }
        };
        let stored: StoredSessions = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(error) => {
                tracing::warn!(%error, "session store is unreadable; starting empty");
                return Ok(0);
            }
        };
        let now = Utc::now();
        let live: HashMap<_, _> = stored
            .sessions
            .into_iter()
            .filter(|(_, record)| record.expires_at > now)
            .collect();
        let count = live.len();
        *self.sessions.write().await = live;
        Ok(count)
    }

    /// Mint a session for `account_id` and return the cookie value to set.
    pub async fn issue(&self, account_id: AccountId) -> Result<String, AppError> {
        let mut random = [0_u8; 32];
        OsRng.fill_bytes(&mut random);
        let handle = general_purpose::URL_SAFE_NO_PAD.encode(random);
        let expires_at = Utc::now() + Duration::days(SESSION_DAYS);

        {
            let mut sessions = self.sessions.write().await;
            let now = Utc::now();
            sessions.retain(|_, record| record.expires_at > now);
            while sessions.len() >= MAX_SESSIONS {
                let oldest = sessions
                    .iter()
                    .min_by_key(|(_, record)| record.expires_at)
                    .map(|(handle, _)| handle.clone());
                match oldest {
                    Some(handle) => {
                        sessions.remove(&handle);
                    }
                    None => break,
                }
            }
            sessions.insert(
                handle.clone(),
                SessionRecord {
                    account_id,
                    expires_at,
                },
            );
        }
        self.persist().await?;
        Ok(handle)
    }

    /// The account a cookie value signs in, if the session is live.
    pub async fn account_for(&self, handle: &str) -> Option<AccountId> {
        if handle.is_empty() {
            return None;
        }
        self.sessions
            .read()
            .await
            .get(handle)
            .filter(|record| record.expires_at > Utc::now())
            .map(|record| record.account_id.clone())
    }

    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }

    async fn persist(&self) -> Result<(), AppError> {
        let stored = StoredSessions {
            sessions: self.sessions.read().await.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("failed to serialize sessions: {error}"))
        })?;
        write_atomically(&self.path, &bytes).await
    }
}

/// Same temp-file-and-rename discipline as the token store: a half-written file
/// would sign everyone out.
async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to create session directory: {error}"
            ))
        })?;
    }
    let mut temporary = path.file_name().unwrap_or_default().to_os_string();
    temporary.push(".tmp");
    let temporary = path.with_file_name(temporary);
    tokio::fs::write(&temporary, bytes).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!("failed to write sessions: {error}"))
    })?;
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("failed to commit sessions: {error}")))
}

/// Pull the session handle out of a `Cookie` header.
///
/// Hand-parsed rather than pulling in a cookie crate: one name, no attributes,
/// no encoding to worry about on the way in.
pub fn session_from_cookie_header(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == COOKIE_NAME).then(|| value.trim())
    })
}

/// The `Set-Cookie` value for a freshly minted session.
///
/// `HttpOnly` keeps it out of page JavaScript, `Secure` keeps it off plain HTTP,
/// and `SameSite=Lax` is enough because the app is served from this same origin
/// — which is the whole reason the frontend is not a separate deployment.
pub fn set_cookie_value(handle: &str, secure: bool) -> String {
    let max_age = SESSION_DAYS * 24 * 60 * 60;
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={handle}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cookie_header_yields_only_its_own_value() {
        assert_eq!(
            session_from_cookie_header("muse_session=abc123"),
            Some("abc123")
        );
        // Real headers carry several cookies, in any order, with loose spacing.
        assert_eq!(
            session_from_cookie_header("other=1; muse_session=abc123; third=2"),
            Some("abc123")
        );
        assert_eq!(
            session_from_cookie_header("  muse_session = spaced  "),
            Some("spaced")
        );
        assert_eq!(session_from_cookie_header("other=1"), None);
        assert_eq!(session_from_cookie_header(""), None);
        // A cookie whose name merely ends in ours must not match.
        assert_eq!(session_from_cookie_header("not_muse_session=x"), None);
    }

    #[test]
    fn the_cookie_cannot_be_read_by_scripts_or_sent_in_the_clear() {
        let cookie = set_cookie_value("handle", true);
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        assert!(cookie.starts_with("muse_session=handle;"), "{cookie}");
        // Local http development would drop a Secure cookie entirely.
        assert!(!set_cookie_value("handle", false).contains("Secure"));
    }

    #[tokio::test]
    async fn a_session_survives_a_restart_and_remembers_its_account() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let account_id = AccountId::new("listener-1");

        let handle = {
            let store = SessionStore::new(path.clone());
            let handle = store.issue(account_id.clone()).await.unwrap();
            assert_eq!(store.account_for(&handle).await, Some(account_id.clone()));
            handle
        };

        // A redeploy must not sign anyone out.
        let reloaded = SessionStore::new(path);
        assert_eq!(reloaded.load().await.unwrap(), 1);
        assert_eq!(reloaded.account_for(&handle).await, Some(account_id));
        assert_eq!(reloaded.account_for("some-other-handle").await, None);
        assert_eq!(reloaded.account_for("").await, None);
    }

    #[tokio::test]
    async fn two_browsers_signing_in_as_different_accounts_stay_distinct() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path().join("sessions.json"));

        let alice = store.issue(AccountId::new("alice")).await.unwrap();
        let bob = store.issue(AccountId::new("bob")).await.unwrap();

        assert_eq!(
            store.account_for(&alice).await,
            Some(AccountId::new("alice"))
        );
        assert_eq!(store.account_for(&bob).await, Some(AccountId::new("bob")));
    }

    #[tokio::test]
    async fn stored_sessions_stay_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path().join("sessions.json"));
        for _ in 0..(MAX_SESSIONS * 2) {
            store.issue(AccountId::new("listener")).await.unwrap();
        }
        assert_eq!(store.count().await, MAX_SESSIONS);
    }

    #[tokio::test]
    async fn a_corrupt_store_starts_empty_rather_than_failing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        tokio::fs::write(&path, b"not json").await.unwrap();
        let store = SessionStore::new(path);
        assert_eq!(store.load().await.unwrap(), 0);
    }
}
