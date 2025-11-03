set dotenv-load

export EDITOR := 'nvim'

alias f := fmt
alias t := test

default:
  just --list

clippy:
  cargo clippy

fmt:
  cargo fmt

test:
  cargo test
  ./bin/py-test
