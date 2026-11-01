pub mod postgres {
    pub use sqlx_postgres::PgPoolOptions;
}

pub use sqlx_core::error::Error;
pub use sqlx_core::query::query;
pub use sqlx_core::query_as::query_as;
pub use sqlx_core::query_builder::QueryBuilder;
pub use sqlx_core::query_scalar::query_scalar;
pub use sqlx_core::transaction::Transaction;
pub use sqlx_postgres::{PgPool, Postgres};
