# Public TLS test fixture

These DER files are public test material, including leaf-key.der. Never use them
as production credentials or add ca.der to the default client trust store.
The leaf is signed by the test CA, has CA:FALSE and serverAuth EKU, and a single
DNS SAN tls-fixture.invalid. Tests use loopback numeric socket addresses without DNS.
The default client rejects this CA. A private test client trusts it only to prove
normal chain verification, original-host SNI and wrong-host rejection.
Generated locally with OpenSSL req/x509/pkcs8; no remote certificate service used.
Byte-exact fixtures are pinned -text in .gitattributes.
