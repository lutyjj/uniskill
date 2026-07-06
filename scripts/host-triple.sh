#!/bin/sh
# Emit Rust target triple for the current host.
arch=$(uname -m | tr 'A-Z' 'a-z')
case "$arch" in
  x86*|amd64) a=x86_64;;
  *) a=aarch64;;
esac
os=$(uname -s | tr 'A-Z' 'a-z')
case "$os" in
  darwin*) p="$a-apple-darwin";;
  linux*)  p="$a-unknown-linux-gnu";;
  *)       p="unknown-$os";;
esac
echo "$p"
