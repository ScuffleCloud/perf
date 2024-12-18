/// A macro helper to implement the `ToSql` and `FromSql` traits for an enum.
/// Unfortunately diesel doesn't automatically generate these for enums, so we
/// have to do it manually. This means we need to make sure that this enum
/// matches the definition in the database.
macro_rules! impl_enum {
    ($enum:ident, $sql_type:ty, {
        $(
            $variant:ident => $value:literal
        ),*$(,)?
    }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, ::diesel::sql_types::SqlType, ::diesel::deserialize::FromSqlRow, ::diesel::expression::AsExpression)]
        #[diesel(sql_type = $sql_type)]
        pub enum $enum {
            $(
                $variant,
            )*
        }

        const _: () = {
            impl ::diesel::serialize::ToSql<$sql_type, ::diesel::pg::Pg> for $enum {
                fn to_sql<'b>(&'b self, out: &mut ::diesel::serialize::Output<'b, '_, ::diesel::pg::Pg>) -> ::diesel::serialize::Result {
                    match self {
                        $(
                            $enum::$variant => ::std::io::Write::write_all(out, $value)?,
                        )*
                    };

                    Ok(::diesel::serialize::IsNull::No)
                }
            }

            impl ::diesel::deserialize::FromSql<$sql_type, ::diesel::pg::Pg> for $enum {
                fn from_sql(bytes: <::diesel::pg::Pg as ::diesel::backend::Backend>::RawValue<'_>) -> ::diesel::deserialize::Result<Self> {
                    match bytes.as_bytes() {
                        $(
                            $value => Ok($enum::$variant),
                        )*
                        bytes => Err(format!("invalid {}: {:?}", stringify!($enum), bytes).into()),
                    }
                }
            }
        };
    };
}

impl_enum!(GithubCiRunStatus, crate::schema::sql_types::GithubCiRunStatus, {
	Queued => b"queued",
	InProgress => b"in_progress",
	Success => b"success",
	Failure => b"failure",
	Cancelled => b"cancelled",
});

impl_enum!(GithubPrStatus, crate::schema::sql_types::GithubPrStatus, {
	Open => b"open",
	Closed => b"closed",
	Draft => b"draft",
});

impl_enum!(GithubPrMergeStatus, crate::schema::sql_types::GithubPrMergeStatus, {
	NotReady => b"not_ready",
	Ready => b"ready",
	Merged => b"merged",
	Conflict => b"conflict",
	CheckFailure => b"check_failure",
	MergeFailure => b"merge_failure",
});

impl_enum!(GithubCiRunStatusCheckStatus, crate::schema::sql_types::GithubCiRunStatusCheckStatus, {
	Pending => b"pending",
	Success => b"success",
	Failure => b"failure",
	Skipped => b"skipped",
});
