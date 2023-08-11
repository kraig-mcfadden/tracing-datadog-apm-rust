.PHONY: default
default: verify

.PHONY: verify
verify:
	cargo check
	cargo test
	cargo fmt
	cargo fix --allow-dirty --allow-staged
	make lint

.PHONY: lint
lint:
	cargo clippy --all-targets --all-features -- -D warnings
