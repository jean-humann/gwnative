# Apple's Developer ID intermediates

Two public certificates, committed rather than downloaded, because a signing
job should not depend on a web server being up — and because a certificate
fetched at run time is one nobody has looked at.

They are Apple's, they are published at
<https://www.apple.com/certificateauthority/>, and they contain no key
material. What they are for is the link between the Developer ID leaf in
`APPLE_DEVELOPER_ID_APPLICATION_P12` and the Apple Root CA the system already
trusts. A Mac with Xcode on it has them; a keychain created from nothing on a
runner does not, and the symptom is `find-identity` reporting the certificate
with `CSSMERR_TP_NOT_TRUSTED` beside it — the identity is *there*, it just
cannot be shown to reach a trust anchor, and `codesign` refuses it.

Both are here rather than only the newer one because Apple does not say which
of the two signs a given leaf, and a certificate issued this year can still
chain through the original CA. Importing the one that is not used costs
nothing.

| File | Subject OU | Expires | SHA-256 |
| --- | --- | --- | --- |
| `AppleDeveloperIDCA.cer` | (none) | 2027-02-01 | `7A:FC:9D:01:A6:2F:03:A2:DE:96:37:93:6D:4A:FE:68:09:0D:2D:E1:8D:03:F2:9C:88:CF:B0:B1:BA:63:58:7F` |
| `AppleDeveloperIDG2CA.cer` | G2 | 2031-09-17 | `F1:6C:D3:C5:4C:7F:83:CE:A4:BF:1A:3E:6A:08:19:C8:AA:A8:E4:A1:52:8F:D1:44:71:5F:35:06:43:D2:DF:3A` |

To check one after replacing it:

```sh
openssl x509 -inform DER -in packaging/certs/AppleDeveloperIDCA.cer \
    -noout -subject -issuer -enddate -fingerprint -sha256
```

The issuer must be `CN=Apple Root CA`. The first of these expires in February
2027; when it does, drop it and leave G2.
