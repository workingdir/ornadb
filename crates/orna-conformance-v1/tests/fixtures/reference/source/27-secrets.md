# 27. Secrets and encrypted files {#secrets}

## Principles

Credentials cannot be plaintext Git data, but encrypted secret documents may be versioned.

**ORNA-SECRET-001** Orna MUST NOT commit plaintext secret values by default.

**ORNA-SECRET-002** Secret values MUST be redacted from Inspect, Display, Present, diagnostics, traces and `sys` unless an explicitly privileged operation requests disclosure.

## SOPS profile

The recommended version 1.0 provider uses SOPS with `age`, PGP or a supported KMS.

```text
example/
├── .sops.yaml
└── secrets/
    └── personal.sops.yaml
```

The private `age` identity or KMS credential remains outside Git.

**ORNA-SOPS-001** An SOPS provider MUST decrypt in memory and SHOULD avoid writing plaintext temporary files.

**ORNA-SOPS-002** A clone without the decryption identity MUST remain able to inspect committed non-secret data and MUST report dependent runs as unavailable rather than corrupt.

## Secret references

```orna
let credential = std.secret.ref("messages.inbox");
google.mail(account: credential)
```

**ORNA-SECRET-003** A `SecretRef` may be displayed and serialized by stable name; the resolved secret value may not.

**ORNA-SECRET-004** `sys.Secret` MUST expose metadata such as name, provider and availability but MUST NOT expose secret contents.

