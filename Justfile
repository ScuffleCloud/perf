mod? local

set shell := ["/bin/bash", "-euo", "pipefail", "-uc"]

# Runs cargo fmt
fmt *args:
    cargo +nightly fmt --all {{args}}

# Runs cargo clippy
lint *args:
    cargo +nightly clippy --fix --allow-dirty --all-targets --all-features --allow-staged {{args}}

# Runs cargo test
test *args:
    #!/bin/bash
    set -euo pipefail

    # We use the nightly toolchain for coverage since it supports branch & no-coverage flags.

    INSTA_FORCE_PASS=1 cargo +nightly llvm-cov clean --workspace
    INSTA_FORCE_PASS=1 cargo +nightly llvm-cov nextest --no-report {{args}}

    # Do not generate the coverage report on CI
    cargo +nightly insta review
    cargo +nightly llvm-cov report --html
    cargo +nightly llvm-cov report --lcov --output-path ./lcov.info

# Runs cargo deny
deny *args:
    cargo deny {{args}} --all-features check

# Update the workspace dependencies
workspace-hack:
    cargo hakari manage-deps
    cargo hakari generate

schema_path := "server/src/database/schema.rs"

# Generate the schema file
diesel-generate: _diesel-generate-unpatched
	touch migrations/schema.patch
	cp migrations/schema.unpatched.rs {{schema_path}}
	just diesel-apply

# Generate the patch file
diesel-patch: _diesel-generate-unpatched
	[ -s {{schema_path}} ] || cp migrations/schema.unpatched.rs {{schema_path}}
	diff -U6 migrations/schema.unpatched.rs {{schema_path}} > migrations/schema.patch || true

# Apply the patch file to the schema file
diesel-apply:
	[ ! -s migrations/schema.patch ] || patch -p0 -o {{schema_path}} --merge < migrations/schema.patch

# Check if the generated schema is up-to-date
diesel-check:
	@ \
		check=$(just _diesel-generate-unpatched-helper 2> /dev/null) && \
		diff -q <(echo "$check") migrations/schema.unpatched.rs > /dev/null || ( \
			echo "The generated schema differs from {{schema_path}}. Run 'just diesel-generate'."; \
			exit 1; \
		)

	@ \
		regex='s/^\(\(\+\+\+\|\-\-\-\)[^\t]*\)\t.*$/\1\t<timestamp>/' && \
		check=$(diff -U6 migrations/schema.unpatched.rs {{schema_path}} | sed "$regex" || echo '') && \
		patch=$(sed "$regex" ./migrations/schema.patch) && \
		diff -q <(echo "$check") <(echo "$patch") > /dev/null || ( \
			echo "The patch file differs from what would be generated. Run 'just diesel-patch'."; \
			exit 1; \
		);

	@echo "Diesel schema and patch are up-to-date!"

_diesel-generate-unpatched:
	just _diesel-generate-unpatched-helper > migrations/schema.unpatched.rs

_diesel-generate-unpatched-helper:
	diesel print-schema --patch-file=<(echo '')
