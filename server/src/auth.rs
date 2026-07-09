use std::time::Duration;

use anyhow::Context;
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const SESSION_COOKIE_NAME: &str = "llm_wiki_session";
pub const SESSION_TTL_SECONDS: i64 = 24 * 60 * 60;
pub const LOGIN_FAILURE_DELAY: Duration = Duration::from_millis(250);

pub fn owner_password_is_acceptable(password: &str) -> bool {
    password.chars().count() >= 8
}

pub async fn hash_password(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|err| anyhow::anyhow!("failed to hash owner password: {err}"))
    })
    .await
    .context("owner password hashing task failed")?
}

pub async fn verify_password(password: String, password_hash: String) -> anyhow::Result<bool> {
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&password_hash)
            .map_err(|err| anyhow::anyhow!("stored owner password hash is invalid: {err}"))?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await
    .context("owner password verification task failed")?
}

pub fn generate_session_token() -> String {
    let mut token = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut token);
    hex::encode(token)
}

pub fn hash_session_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn session_cookie(token: &str) -> String {
    format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; Max-Age={SESSION_TTL_SECONDS}; HttpOnly; SameSite=Lax"
    )
}

pub fn expired_session_cookie() -> String {
    format!("{SESSION_COOKIE_NAME}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax")
}
