#!/usr/bin/env bash
set -e

# Fail build, if there are any warnings.
export RUSTFLAGS="-D warnings"

(
    cd messages
    cargo test --verbose)
(
    cd firmware-lib
    cargo test --verbose)
(
    cd host-lib
    cargo test --verbose)
(
    cd test-firmware
    cargo build --verbose)
(
    cd test-suite
    cargo build --tests --verbose)
