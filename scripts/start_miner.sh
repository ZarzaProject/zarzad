#!/bin/bash
cd "$(dirname "$0")/.."
cargo run --bin zarza-miner -- --address $1
