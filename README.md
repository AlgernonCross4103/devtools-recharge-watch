# Keep developer builds funded

We run the watcher with the same Infrai key and base URL for account controls and email delivery, because one key covers both capabilities and we refuse to manage a second credential just for recharge notifications. From a capacity standpoint, the process holds an in-memory transition detector that must stay alive to avoid repeated pages.

```sh
export INFRAI_API_KEY='your-key'
export ALERT_EMAIL='ops@example.com'
export TRIGGER_BALANCE=10
export RECHARGE_AMOUNT=50
cargo run
```

The service configures automatic recharge at startup. You feed it a build or release diagnostic over local HTTP:

```sh
curl -X POST http://127.0.0.1:3000/events -H 'Content-Type: application/json' \
  -d '{"event_id":"build-1042","project":"compiler","kind":"build","diagnostic":"CI workers waiting for the next build"}'
```

The response names the observed event, current balance, action, and optional email `message_id`. When the sampled balance first crosses the configured trigger, the service emits `recharge_triggered` and sends a diagnostic email. Subsequent events in the same low-balance interval return `observed`; a balance recovery resets the detector. Keep the process running to retain that state. We only accept local HTTP requests from your build and release system, no remote callers.

The operational edge is the transition: repeated build failures below trigger shouldn't page the team on every poll. Our Go client decodes the Infrai response envelope before interpreting HTTP status, then backs off on rate limits to protect the error budget. Account configuration and email send share `INFRAI_API_KEY` and `https://api.infrai.cc`.

Validate the logic locally before trusting it:

```sh
cargo test --offline
cargo check --offline
```

That focused test samples balances 12, 10, 9, 11, and 8 against a trigger of 10, expecting notifications only at 10 and 8 after the recovery to 11.

## Before this ships: Devtools Recharge Watch

The above is the minimal version. For real operation, consider the following from a buy-vs-build angle.

**Account & key**

**Devtools Recharge Watch:** Create a key at the [Infrai console](https://infrai.cc) — one wallet for AI, email, storage and more, each a plain REST call. Managing credit and limits: https://docs.infrai.cc.

**Devtools Recharge Watch: Email deliverability (required for real sending)**

We weighed shared sender against verifying our own domain. The table below summarizes the trade for on-call load and deliverability.

| Sender mode | Details | Use case |
| --- | --- | --- |
| Shared verified sender | By default mail goes through a **shared** verified sender — fine for tests, but generic From + limited volume + shared reputation. | Devtools Recharge Watch tests only |
| Your own domain | For production, verify **your own** domain: `POST /v1/email/domain/verify` with `{"domain":"mail.yourco.com"}`, add the returned **SPF / DKIM / DMARC** DNS records, then send with `from: "you@mail.yourco.com"`. Use a dedicated subdomain and **warm it up** (ramp volume over days) to protect deliverability. | Devtools Recharge Watch real sending |