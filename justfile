set dotenv-load

export EDITOR := 'nvim'

alias f := fmt
alias t := test

default:
  just --list

ci: fmt-check clippy test

clippy:
  cargo clippy --all --all-targets

fmt:
  cargo fmt --all

fmt-check:
  cargo fmt --all --check

test:
  cargo test
  ./bin/py-test
