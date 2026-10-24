# Java certificate distrust data

`blocked.certs` is the vendor-provided SHA-256 certificate distrust list from
<https://ops.java/unix/blocked.certs>, retrieved on 2026-10-22 for the OpenJDK
[eMudhra security update](https://openjdk.org/groups/vulnerability/advisories/2026-10-21).
It is used by the standard JDK certificate validator, not an application TLS override.

CI and the Iris release workflow install this file into their disposable JDK. Local Iris
tasks refuse a JDK missing these entries rather than modifying a shared installation.
Install a vendor security-updated JDK, or apply the vendor's documented `blocked.certs`
update to your own JDK, before running them. Temurin 21.0.13 already omits the eMudhra roots
that [OpenJDK's guidance](https://openjdk.org/emudhra) says to delete.

Update this snapshot from the vendor when its distrust data changes. Preserve the
`Algorithm=SHA-256` header and every existing entry. Do not replace it with a
hand-maintained list or disable certificate/hostname verification.
