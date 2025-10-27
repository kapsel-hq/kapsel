//! Repository for API key database operations.
//!
//! Manages API key lifecycle including creation, validation, revocation, and
//! expiration handling. All API keys are hashed using SHA256 before storage
//! for security.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{error::Result, models::TenantId};

/// API key data structure for database operations.
#[derive(Debug, Clone)]
pub struct ApiKey {
    /// Unique identifier for the API key
    pub id: Uuid,
    /// ID of the tenant that owns this API key
    pub tenant_id: TenantId,
    /// Hashed representation of the API key for secure storage
    pub key_hash: String,
    /// Human-readable name for the API key
    pub name: String,
    /// Optional expiration timestamp for the API key
    pub expires_at: Option<DateTime<Utc>>,
    /// Optional revocation timestamp when the key was disabled
    pub revoked_at: Option<DateTime<Utc>>,
    /// Timestamp of the last time this API key was used for authentication
    pub last_used_at: Option<DateTime<Utc>>,
    /// Timestamp when the API key was created
    pub created_at: DateTime<Utc>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ApiKey {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            tenant_id: TenantId(row.try_get("tenant_id")?),
            key_hash: row.try_get("key_hash")?,
            name: row.try_get("name")?,
            expires_at: row.try_get("expires_at")?,
            revoked_at: row.try_get("revoked_at")?,
            last_used_at: row.try_get("last_used_at")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// Repository for API key database operations.
///
/// Provides type-safe access to API key data with support for transactional
/// operations, validation, and security features like revocation and
/// expiration.
pub struct Repository {
    pool: Arc<PgPool>,
}

impl Repository {
    /// Creates a new repository instance.
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// Returns a reference to the database pool.
    pub fn pool(&self) -> Arc<PgPool> {
        self.pool.clone()
    }

    /// Creates a new API key.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails or constraints are violated.
    pub async fn create(&self, api_key: &ApiKey) -> Result<()> {
        self.create_impl(&*self.pool, api_key).await
    }

    /// Creates an API key within a transaction.
    ///
    /// # Errors
    ///
    /// Returns error if insert fails.
    pub async fn create_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        api_key: &ApiKey,
    ) -> Result<()> {
        self.create_impl(&mut **tx, api_key).await
    }

    /// Private helper for creating API keys with generic executor.
    async fn create_impl<'e, E>(&self, executor: E, api_key: &ApiKey) -> Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        sqlx::query(
            r"
            INSERT INTO api_keys (id, tenant_id, key_hash, name, expires_at, revoked_at, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ",
        )
        .bind(api_key.id)
        .bind(api_key.tenant_id.0)
        .bind(&api_key.key_hash)
        .bind(&api_key.name)
        .bind(api_key.expires_at)
        .bind(api_key.revoked_at)
        .bind(api_key.created_at)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Finds an API key by its hash.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>> {
        let api_key = sqlx::query_as::<_, ApiKey>(
            r"
            SELECT id, tenant_id, key_hash, name, expires_at, revoked_at,
                   last_used_at, created_at
            FROM api_keys
            WHERE key_hash = $1
            ",
        )
        .bind(key_hash)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(api_key)
    }

    /// Validates an API key and updates last_used_at timestamp.
    ///
    /// Checks that the API key exists, is not revoked, and is not expired.
    /// If validation succeeds, updates the last_used_at timestamp.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn validate(&self, key_hash: &str) -> Result<Option<TenantId>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            r"
            SELECT tenant_id
            FROM api_keys
            WHERE key_hash = $1
              AND revoked_at IS NULL
              AND (expires_at IS NULL OR expires_at > NOW())
            ",
        )
        .bind(key_hash)
        .fetch_optional(&*self.pool)
        .await?;

        if let Some((tenant_id,)) = row {
            // Update last_used_at timestamp
            let _ = sqlx::query(
                r"
                UPDATE api_keys
                SET last_used_at = NOW()
                WHERE key_hash = $1
                ",
            )
            .bind(key_hash)
            .execute(&*self.pool)
            .await;

            Ok(Some(TenantId(tenant_id)))
        } else {
            Ok(None)
        }
    }

    /// Revokes an API key by setting revoked_at timestamp.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn revoke(&self, key_hash: &str) -> Result<()> {
        sqlx::query(
            r"
            UPDATE api_keys
            SET revoked_at = NOW()
            WHERE key_hash = $1
            ",
        )
        .bind(key_hash)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Sets expiration date for an API key.
    ///
    /// # Errors
    ///
    /// Returns error if update fails.
    pub async fn set_expiration(
        &self,
        key_hash: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        sqlx::query(
            r"
            UPDATE api_keys
            SET expires_at = $2
            WHERE key_hash = $1
            ",
        )
        .bind(key_hash)
        .bind(expires_at)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Finds all API keys for a tenant.
    ///
    /// Returns only non-revoked keys unless include_revoked is true.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn find_by_tenant(
        &self,
        tenant_id: TenantId,
        include_revoked: bool,
    ) -> Result<Vec<ApiKey>> {
        let query = if include_revoked {
            r"
            SELECT id, tenant_id, key_hash, name, expires_at, revoked_at,
                   last_used_at, created_at
            FROM api_keys
            WHERE tenant_id = $1
            ORDER BY created_at DESC
            "
        } else {
            r"
            SELECT id, tenant_id, key_hash, name, expires_at, revoked_at,
                   last_used_at, created_at
            FROM api_keys
            WHERE tenant_id = $1 AND revoked_at IS NULL
            ORDER BY created_at DESC
            "
        };

        let api_keys =
            sqlx::query_as::<_, ApiKey>(query).bind(tenant_id.0).fetch_all(&*self.pool).await?;

        Ok(api_keys)
    }

    /// Deletes an API key.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn delete(&self, key_hash: &str) -> Result<()> {
        sqlx::query(
            r"
            DELETE FROM api_keys
            WHERE key_hash = $1
            ",
        )
        .bind(key_hash)
        .execute(&*self.pool)
        .await?;

        Ok(())
    }

    /// Counts API keys for a tenant.
    ///
    /// # Errors
    ///
    /// Returns error if query fails.
    pub async fn count_by_tenant(&self, tenant_id: TenantId, include_revoked: bool) -> Result<i64> {
        let query = if include_revoked {
            r"
            SELECT COUNT(*) FROM api_keys
            WHERE tenant_id = $1
            "
        } else {
            r"
            SELECT COUNT(*) FROM api_keys
            WHERE tenant_id = $1 AND revoked_at IS NULL
            "
        };

        let count: (i64,) = sqlx::query_as(query).bind(tenant_id.0).fetch_one(&*self.pool).await?;

        Ok(count.0)
    }

    /// Cleans up expired API keys by deleting them.
    ///
    /// This is typically run as a background job to remove expired keys
    /// and reclaim storage space.
    ///
    /// # Errors
    ///
    /// Returns error if delete fails.
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM api_keys
            WHERE expires_at IS NOT NULL AND expires_at < NOW()
            ",
        )
        .execute(&*self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repository_can_be_created() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://test").unwrap();
        let _repo = Repository::new(Arc::new(pool));
    }
}
