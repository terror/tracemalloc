set dotenv-load

export EDITOR := 'nvim'

alias f := fmt

default:
  just --list

fmt:
  cargo fmt
