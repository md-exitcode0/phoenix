# Passes

Passes is Phoenix's local store for **logins, payment cards, API keys, tokens,
verification codes, and free-form secrets**. It replaces the old "Vault"
settings page and its storage format.

## Rules

* **Saving never needs a password.** A pass is sealed to a public key.
* **Viewing or using a secret needs Passes unlocked**, once per Phoenix run.
  The key stays in the gateway's memory (zeroized on drop) until Phoenix fully
  quits from the tray, the user presses *Lock*, or an optional idle auto-lock
  (`security.vault_auto_lock_minutes`, default 0 = never) fires.
* **Secrets never enter model context.** Agents see ids and public metadata
  only; the runtime types secrets into browser fields or HTTP headers itself.

## Encryption (store version 3, `~/.phoenix/vault/`)

| File | Contents |
|---|---|
| `config.json` | version 3, X25519 **public key**, private key wrapped under `K`, Argon2id salt + cost params, `K` wrapped by the password key, `K` wrapped by the recovery key |
| `passes.json` | records: plaintext metadata + sealed secret box |
| `setup.key` | `K` itself, **only** until a master password is set |
| `*.v2*.bak` | encrypted backups of a migrated version-2 vault |

* `K`: random 32 bytes. Wrapped with AES-256-GCM by
  `Argon2id(master password, salt, m=64 MiB, t=3, p=1)` and by the recovery key
  (`PHX1-…`, shown once). Changing the password re-wraps only `K`.
* X25519 private key: AES-256-GCM under `K`, AAD binds the public key; on
  unlock the derived public key must match the stored one.
* Each pass: ephemeral X25519 → shared secret → HKDF-SHA256
  (salt = ephemeral‖recipient public, info `phoenix-passes-v3/seal`) →
  XChaCha20-Poly1305 with a random 24-byte nonce. AAD binds the pass id,
  owner scope, site, and kind, so a sealed secret cannot be moved to another
  pass or re-pointed at another site.
* Plaintext metadata: title, site, username, kind, timestamps, and computed
  public hints (card brand + last 4, an API key's last 4, names of secondary
  fields). Card CVC/expiry/name/billing ZIP, TOTP seeds, and every primary
  secret are sealed.

### Migration from version 2

The old data key becomes `K`, so the existing master password and recovery key
keep working. If the old device-local `agent-runtime.key` exists, migration runs
immediately (and deletes that plaintext key file); otherwise on the first
unlock. Records are re-sealed, the old files are kept as encrypted `.bak`
copies, and nothing is lost.

## Agent tools

* `credential_list` — metadata only; filters by site and kind; works locked.
* `ask_for_pass` — typed inline popup (`login`, `card`, `api_key`, `token`,
  `verification_code`, `secret`) with agent-chosen title, reason, site,
  fields, and labels. The UI seals the entry via the gateway's
  `Vault { action: "fulfill_request" }`; the agent receives only the receipt.
  The turn waits up to 5 minutes; a later answer still wakes the agent.
* `pass_use` — `browser_field` (any field: password, username, totp → current
  6-digit code, card number/cvc/expiry/exp_month/exp_year/name/billing_zip,
  key, value, code) or `http_header` (https only, host must match the pass's
  site or saved base URL, response scrubbed of the secret). One-time codes are
  deleted after their first fill.
* If a pass is used while Passes is locked, the runtime shows the one-time
  unlock card and waits.

## Desktop lifecycle

Closing the window hides Phoenix to the system tray; the gateway daemon and
every agent's browser keep running. The tray offers *Open Phoenix*, a status
line, and *Quit Phoenix*. Only *Quit Phoenix* stops the gateway (and with it
drops the unlocked key). `PHOENIX_CLOSE_TO_TRAY=0` restores quit-on-close.
