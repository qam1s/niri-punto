default: test

test:
    cargo test --locked

lint:
    cargo clippy --all-targets -- -D warnings
    cargo fmt --check

release version:
    #!/usr/bin/env bash
    set -euo pipefail
    sed -i '0,/^version = ".*"$/s//version = "{{ version }}"/' Cargo.toml
    cargo generate-lockfile --offline
    cargo test --locked
    git add Cargo.toml Cargo.lock
    git commit -m "chore: bump package version"
    git tag "v{{ version }}"
