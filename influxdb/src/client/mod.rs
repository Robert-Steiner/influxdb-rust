//! Client which can read and write data from InfluxDB.
//!
//! # Arguments
//!
//!  * `url`: The URL where InfluxDB is running (ex. `http://localhost:8086`).
//!  * `database`: The Database against which queries and writes will be run.
//!
//! # Examples
//!
//! ```rust
//! use influxdb::Client;
//!
//! let client = Client::new("http://localhost:8086", "test");
//!
//! assert_eq!(client.database_name(), "test");
//! ```

use futures::prelude::*;
use reqwest::{self, Client as ReqwestClient, StatusCode};

use crate::query::QueryTypes;
use crate::Error;
use crate::Query;
use std::sync::Arc;

#[derive(Clone, Debug)]
/// Internal Representation of a Client
pub struct Client {
    pub(crate) url: Arc<String>,
    pub(crate) parameters: Vec<(&'static str, String)>,
    pub(crate) client: ReqwestClient,
}

impl Client {
    /// Instantiates a new [`Client`](crate::Client)
    ///
    /// # Arguments
    ///
    ///  * `url`: The URL where InfluxDB is running (ex. `http://localhost:8086`).
    ///  * `database`: The Database against which queries and writes will be run.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use influxdb::Client;
    ///
    /// let _client = Client::new("http://localhost:8086", "test");
    /// ```
    pub fn new<S1, S2>(url: S1, database: S2) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        Client {
            url: Arc::new(url.into()),
            parameters: vec![("db", database.into())],
            client: ReqwestClient::new(),
        }
    }

    /// Add authentication/authorization information to [`Client`](crate::Client)
    ///
    /// # Arguments
    ///
    /// * username: The Username for InfluxDB.
    /// * password: The Password for the user.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use influxdb::Client;
    ///
    /// let _client = Client::new("http://localhost:9086", "test").with_auth("admin", "password");
    /// ```
    pub fn with_auth<S1, S2>(mut self, username: S1, password: S2) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        self.parameters.push(("u", username.into()));
        self.parameters.push(("p", password.into()));

        self
    }

    /// Returns the name of the database the client is using
    pub fn database_name(&self) -> &str {
        // safe to unwrap because we set the database in `new`
        &self.parameters.first().unwrap().1
    }

    /// Returns the URL of the InfluxDB installation the client is using
    pub fn database_url(&self) -> &str {
        &self.url
    }

    /// Pings the InfluxDB Server
    ///
    /// Returns a tuple of build type and version number
    pub async fn ping(&self) -> Result<(String, String), Error> {
        let url = &format!("{}/ping", self.url);
        let res = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|err| Error::ProtocolError {
                error: format!("{}", err),
            })?;

        let build = res
            .headers()
            .get("X-Influxdb-Build")
            .unwrap()
            .to_str()
            .unwrap();
        let version = res
            .headers()
            .get("X-Influxdb-Version")
            .unwrap()
            .to_str()
            .unwrap();

        Ok((build.to_owned(), version.to_owned()))
    }

    /// Sends a [`ReadQuery`](crate::ReadQuery) or [`WriteQuery`](crate::WriteQuery) to the InfluxDB Server.
    ///
    /// A version capable of parsing the returned string is available under the [serde_integration](crate::integrations::serde_integration)
    ///
    /// # Arguments
    ///
    ///  * `q`: Query of type [`ReadQuery`](crate::ReadQuery) or [`WriteQuery`](crate::WriteQuery)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use influxdb::{Client, Query, Timestamp};
    /// use influxdb::InfluxDbWriteable;
    /// use std::time::{SystemTime, UNIX_EPOCH};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), influxdb::Error> {
    /// let start = SystemTime::now();
    /// let since_the_epoch = start
    ///   .duration_since(UNIX_EPOCH)
    ///   .expect("Time went backwards")
    ///   .as_millis();
    ///
    /// let client = Client::new("http://localhost:8086", "test");
    /// let query = Timestamp::Milliseconds(since_the_epoch)
    ///     .into_query("weather")
    ///     .add_field("temperature", 82);
    /// let results = client.query(&query).await?;
    ///
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// If the function can not finish the query,
    /// a [`Error`] variant will be returned.
    ///
    /// [`Error`]: enum.Error.html
    pub async fn query<'q, Q>(&self, q: &'q Q) -> Result<String, Error>
    where
        Q: Query,
        &'q Q: Into<QueryTypes<'q>>,
    {
        let query = q.build().map_err(|err| Error::InvalidQueryError {
            error: format!("{}", err),
        })?;

        let request_builder = match q.into() {
            QueryTypes::Read(_) => {
                let read_query = query.get();
                let url = &format!("{}/query", self.url);
                let query = [("q", &read_query)];

                if read_query.contains("SELECT") || read_query.contains("SHOW") {
                    self.client.get(url).query(&self.parameters).query(&query)
                } else {
                    self.client.post(url).query(&self.parameters).query(&query)
                }
            }
            QueryTypes::Write(write_query) => {
                let url = &format!("{}/write", self.url);
                let precision = [("precision", write_query.get_precision())];

                self.client
                    .post(url)
                    .query(&self.parameters)
                    .query(&precision)
                    .body(query.get())
            }
        };

        let request = request_builder
            .build()
            .map_err(|err| Error::UrlConstructionError {
                error: format!("{}", err),
            })?;

        let res = self
            .client
            .execute(request)
            .map_err(|err| Error::ConnectionError { error: err })
            .await?;

        match res.status() {
            StatusCode::UNAUTHORIZED => return Err(Error::AuthorizationError),
            StatusCode::FORBIDDEN => return Err(Error::AuthenticationError),
            _ => {}
        }

        let s = res.text().await.map_err(|_| Error::DeserializationError {
            error: "response could not be converted to UTF-8".to_string(),
        })?;

        // todo: improve error parsing without serde
        if s.contains("\"error\"") {
            return Err(Error::DatabaseError {
                error: format!("influxdb error: \"{}\"", s),
            });
        }

        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::Client;

    #[test]
    fn test_fn_database() {
        let client = Client::new("http://localhost:8068", "database");
        assert_eq!(client.database_name(), "database");
        assert_eq!(client.database_url(), "http://localhost:8068");
    }

    #[test]
    fn test_with_auth() {
        let client = Client::new("http://localhost:8068", "database");
        assert_eq!(vec![("db", "database".to_string())], client.parameters);

        let with_auth = client.with_auth("username", "password");
        assert_eq!(
            vec![
                ("db", "database".to_string()),
                ("u", "username".to_string()),
                ("p", "password".to_string())
            ],
            with_auth.parameters
        );
    }
}
