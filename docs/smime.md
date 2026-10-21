# S/MIME: signed and encrypted mail

MailCraft signs, encrypts, verifies and decrypts mail with S/MIME (RFC 8551). Everything is done in the app
in pure Rust (`crates/smime`, RustCrypto); no OpenSSL and no system crypto service is needed, so the desktop
and the browser builds behave the same.

## Digital IDs

A digital ID is a certificate for your address plus its private key, usually delivered by a certificate
authority as a `.pfx` / `.p12` file protected by a password.

- **File › Options › Trust Center › Email Security › Digital IDs...** lists your digital IDs, the
  certificates of other people, and the certificates you trust.
- **Import...** takes a `.pfx` / `.p12` file (with its password), or a PEM private key file and its
  certificate. Tick *Also trust the authority that issued it* when the authority is your organisation's own.
- **Export...** writes a digital ID back to a `.pfx` file (PBES2 / AES-256, readable by current Windows,
  macOS, Thunderbird and OpenSSL) under a new password.
- Commands: `options.digitalIds`, `options.importDigitalId`, `options.exportDigitalId`,
  `options.removeDigitalId`, `options.setDefaultDigitalId`.

**Where keys are kept.** `certificates.json` in the profile folder holds the certificates and each private
key *sealed* (an encrypted PKCS #8). The secret that opens a sealed key is stored with the account passwords:
in the operating system's keyring when MailCraft uses it, otherwise in `secrets.json` (see
[accounts.md](accounts.md)). Decrypted messages are never written to disk; they are opened in memory when shown.

## Sending

- **Options › Encrypt** and **Options › Sign** in the message window (`compose.encrypt`, `compose.sign`).
- Signing uses your digital ID for the sending address: SHA-256, RSA, the signing time and your certificate
  chain. By default the message is *clear-signed* (`multipart/signed`): people without S/MIME can still read
  it.
- Encryption uses AES-256-CBC with RSA key transport for every To, Cc and Bcc recipient, and for you, so the
  copy in Sent Items stays readable. A recipient without a known certificate stops the send with a choice:
  **Send Unencrypted** or **Cancel**.
- Recipients' certificates come from valid signed mail they send you, from the `KEY` of their contact card
  (vCard), or from a file you import (`options.importCertificate`).
- Trust Center › Email Security options: encrypt every new message, sign every new message, and send signed
  messages as clear text (off sends opaque signed data). A reply to an encrypted message starts encrypted.

## Reading

The reading pane and read window open signed and encrypted messages and say what they found in the info bar:

- *Encrypted: only the recipients of this message can read it.*, or why it cannot be opened (no digital ID it
  was encrypted for).
- *Signed by Name &lt;address&gt;.* with the status: valid; not trusted (the certificate does not lead to a
  root your system trusts or one you chose to trust); certificate not valid (expired); not valid (the message
  was changed after it was signed). A certificate issued to another address than the sender's is flagged.
- **Details...** opens Message Security (`mail.signatureDetails`): signer, status and reason, signing time,
  digest, and the certificate chain with fingerprints. **Trust the Issuer** trusts the chain's root
  (`options.trustCertificate`).

Replies, forwards and Save Attachments use the opened message.

## Supported formats

Read: SignedData with RSA PKCS #1 v1.5 or RSASSA-PSS over SHA-1/SHA-2, signers by issuer and serial or by
subject key identifier; EnvelopedData with AES-128/192/256-CBC or triple DES and RSA PKCS #1 v1.5 or
RSAES-OAEP key transport; BER (streamed) and DER encodings; `.pfx` files using PBES2 or the legacy PKCS #12
schemes (SHA-1 with triple DES or RC2). Not supported (reported, never a crash): elliptic-curve keys, key
agreement, AES-GCM (AuthEnvelopedData), S/MIME receipts, revocation checking (CRL / OCSP).
