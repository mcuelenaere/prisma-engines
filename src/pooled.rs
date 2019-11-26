mod manager;

use crate::{
    ast,
    connector::{self, ConnectionInfo, Queryable, SqlFamily, DBIO},
};
use manager::{PooledConnection, QuaintManager};
use mobc::Pool;
use std::convert::TryFrom;
use url::Url;

/// The main entry point and an abstraction over database connections and
pub struct Quaint {
    pub inner: Pool<QuaintManager>,
    connection_info: ConnectionInfo,
}

impl Quaint {
    /// Create a new pool of connections to the database. The connection string
    /// follows the specified format:
    ///
    /// `connector_type://user:password@host/database?parameters`
    ///
    /// Connector type can be one of the following:
    ///
    /// - `sqlite`/`file` opens an SQLite connection
    /// - `mysql` opens a MySQL connection
    /// - `postgres`/`postgresql` opens a PostgreSQL connection
    ///
    /// All parameters should be given in the query string format:
    /// `?key1=val1&key2=val2`. All parameters are optional.
    ///
    /// Common parameters:
    ///
    /// - `connection_limit` defines the number of connections opened to the
    /// database. If not set, defaults to the [HikariCP
    /// Recommendation](https://github.com/brettwooldridge/HikariCP/wiki/About-Pool-Sizing):
    /// `physical_cpus * 2 + 1`.
    ///
    /// SQLite:
    ///
    /// - `user`/`password` do not do anything and can be emitted.
    /// - `host` should point to the database file.
    /// - `db_name` parameter should give a name to the database attached for
    ///   query namespacing.
    ///
    /// PostgreSQL:
    ///
    /// - `sslmode` either `disable`, `prefer` or `require`. [Read more](https://docs.rs/tokio-postgres/0.5.0-alpha.1/tokio_postgres/config/enum.SslMode.html)
    /// - `sslcert` should point to a PEM certificate file.
    /// - `sslidentity` should point to a PKCS12 certificate database.
    /// - `sslpassword` the password to open the PKCS12 database.
    /// - `sslaccept` either `strict` or `accept_invalid_certs`. If strict, the
    ///   certificate needs to be valid and in the CA certificates.
    ///   `accept_invalid_certs` accepts any certificate from the server and can
    ///   lead to weakened security. Defaults to `strict`.
    /// - `schema` the default search path.
    /// - `host` additionally the host can be given as a parameter, typically in
    ///   cases when connectiong to the database through a unix socket to
    ///   separate the database name from the database path, such as
    ///   `postgresql:///dbname?host=/var/run/postgresql`.
    ///
    /// MySQL:
    ///
    /// - `sslcert` should point to a PEM certificate file.
    /// - `sslidentity` should point to a PKCS12 certificate database.
    /// - `sslpassword` the password to open the PKCS12 database.
    /// - `sslaccept` either `strict` or `accept_invalid_certs`. If strict, the
    ///   certificate needs to be valid and in the CA certificates.
    ///   `accept_invalid_certs` accepts any certificate from the server and can
    ///   lead to weakened security. Defaults to `strict`.
    /// - `socket` needed when connecting to MySQL database through a unix
    ///   socket. When set, the host parameter is dismissed.
    pub async fn new(url_str: &str) -> crate::Result<Self> {
        let url = Url::parse(url_str)?;

        let (manager, connection_limit) = match url.scheme() {
            #[cfg(feature = "sqlite")]
            "file" | "sqlite" => {
                let params = crate::connector::SqliteParams::try_from(url_str)?;

                let manager = QuaintManager::Sqlite {
                    file_path: params.file_path,
                    db_name: params.db_name,
                };

                (manager, params.connection_limit)
            }
            #[cfg(feature = "mysql")]
            "mysql" => {
                let url = crate::connector::MysqlUrl::new(url)?;
                let connection_limit = url.connection_limit();
                let manager = QuaintManager::Mysql(url.clone());

                (manager, connection_limit as u32)
            }
            #[cfg(feature = "postgresql")]
            "postgres" | "postgresql" => {
                let url = crate::connector::PostgresUrl::new(url)?;
                let connection_limit = url.connection_limit();
                let manager = QuaintManager::Postgres(url);

                (manager, connection_limit as u32)
            }
            _ => unimplemented!("Supported url schemes: file or sqlite, mysql, postgres or postgresql."),
        };

        let connection_info = ConnectionInfo::from_url(url_str)?;
        Self::log_start(connection_info.sql_family(), connection_limit);

        let inner = Pool::builder().max_size(connection_limit).build(manager).await?;

        Ok(Self { inner, connection_info })
    }

    /// The number of connections in the pool.
    pub async fn capacity(&self) -> u32 {
        self.inner.state().await.connections
    }

    /// Reserve a connection from the pool.
    pub async fn check_out(&self) -> crate::Result<PooledConnection> {
        Ok(PooledConnection {
            inner: self.inner.get().await?,
        })
    }

    /// Info about the connection and underlying database.
    pub fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }

    fn log_start(family: SqlFamily, connection_limit: u32) {
        #[cfg(not(feature = "tracing-log"))]
        {
            info!("Starting a {} pool with {} connections.", family, connection_limit);
        }
        #[cfg(feature = "tracing-log")]
        {
            tracing::info!("Starting a {} pool with {} connections.", family, connection_limit);
        }
    }
}

impl Queryable for Quaint {
    fn execute<'a>(&'a self, q: ast::Query<'a>) -> DBIO<'a, Option<ast::Id>> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.execute(q).await
        })
    }

    fn query<'a>(&'a self, q: ast::Query<'a>) -> DBIO<'a, connector::ResultSet> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.query(q).await
        })
    }

    fn query_raw<'a>(&'a self, sql: &'a str, params: &'a [ast::ParameterizedValue]) -> DBIO<'a, connector::ResultSet> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.query_raw(sql, params).await
        })
    }

    fn execute_raw<'a>(&'a self, sql: &'a str, params: &'a [ast::ParameterizedValue]) -> DBIO<'a, u64> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.execute_raw(sql, params).await
        })
    }

    fn turn_off_fk_constraints(&self) -> DBIO<()> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.turn_off_fk_constraints().await
        })
    }

    fn turn_on_fk_constraints(&self) -> DBIO<()> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.turn_on_fk_constraints().await
        })
    }

    fn raw_cmd<'a>(&'a self, cmd: &'a str) -> DBIO<'a, ()> {
        DBIO::new(async move {
            let conn = self.check_out().await?;
            conn.raw_cmd(cmd).await
        })
    }
}
