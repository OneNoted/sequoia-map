#!/bin/sh
# Fails when the JDK in JAVA_HOME lacks the vendor distrust entries; see config/security/README.md.
set -eu
jdk_list=$JAVA_HOME/lib/security/blocked.certs
outdated() {
  printf 'JDK certificate distrust data is outdated; see config/security/README.md\n' >&2
  exit 1
}
# The JDK ignores every entry unless the file declares the fingerprint algorithm.
grep -qxF 'Algorithm=SHA-256' "$jdk_list" || outdated
while IFS= read -r fingerprint; do
  case "$fingerprint" in Algorithm=*|\#*|'') continue ;; esac
  grep -qxF "$fingerprint" "$jdk_list" || outdated
done < "$(dirname "$0")/../config/security/blocked.certs"
