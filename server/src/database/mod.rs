pub mod ci_run;
pub mod ci_run_check;
pub mod enums;
pub mod pr;
pub mod schema;

#[cfg(test)]
macro_rules! test_query {
    (name: $name:ident, query: $query:expr, expected: $($expected:tt)*) => {
        #[test]
        fn $name() {
            let query = ::diesel::debug_query::<Pg, _>(&$query).to_string();
            ::insta::assert_snapshot!(query, $($expected)*);
        }
    };
}

#[cfg(test)]
use test_query;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
async fn get_connection() -> diesel_async::AsyncPgConnection {
    use diesel_async::AsyncConnection;

    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let mut conn = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        diesel_async::AsyncPgConnection::establish(&db_url),
    )
    .await
    .expect("timeout connecting to database")
    .expect("failed to connect to database");

    conn.begin_test_transaction().await.expect("failed to begin test transaction");

    conn
}
