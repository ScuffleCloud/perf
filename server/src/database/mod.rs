pub mod ci_run;
pub mod ci_run_check;
pub mod enums;
pub mod pr;
pub mod schema;

#[cfg(test)]
macro_rules! test_query {
    (name: $name:ident, query: $query:expr, $(expected: $($expected:tt)*)?) => {
        #[test]
        fn $name() {
            fn debug(query: impl ::diesel::query_builder::QueryFragment<diesel::pg::Pg>) -> String {
                ::sqlformat::format(
                    &::diesel::debug_query::<diesel::pg::Pg, _>(&query).to_string(),
                    &::sqlformat::QueryParams::None,
                    &::sqlformat::FormatOptions::default(),
                )
            }

            ::insta::assert_snapshot!(debug($query)$(, $($expected)*)?);
        }
    };
}

#[cfg(test)]
use test_query;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn get_test_connection() -> diesel_async::AsyncPgConnection {
    use diesel_async::AsyncConnection;

    tracing_subscriber::fmt()
        .with_file(true)
        .with_line_number(true)
        .with_target(true)
        .with_level(true)
        .init();

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
