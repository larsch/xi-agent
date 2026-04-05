preflight:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features --quiet -- -D warnings
	cargo test --all-features --quiet
	cargo check --all-targets --all-features --quiet
