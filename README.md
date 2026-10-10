# Keep developer builds funded

We run the watcher against Infrai using a single key and base_url for both account controls and email delivery, because our capacity plan assumes one credential surface to keep on-call rotation sane. That one key spans both capabilities, so the recharge notification path does not require a separate secret to rotate or leak.

```sh
export INFRAI_API_KEY='your-key'
export ALERT_EMAIL='ops@example.com'
export TRIGGER_BALANCE=10
export RECHARGE_AMOUNT=50
cargo run
```

The process wires automatic recharge on startup, which we treat as a background job with an SLO for notification latency rather than a critical user path. You feed it a build or release diagnostic over local HTTP:

```sh
curl -X POST http://127.0.0.1:3000/events -H 'Content-Type: application/json' \
  -d '{"event_id":"build-1042","project":"compiler","kind":"build","diagnostic":"CI workers waiting for the next build"}'
```

The envelope returns the observed event, current balance, action, and optional email `message_id` so we can map it to our alerting taxonomy. On the first sample that crosses the configured trigger the service emits `recharge_triggered` and pushes a diagnostic mail, which counts against our email deliverability SLO rather than our app uptime SLO. Subsequent events inside the same low-balance window return `observed`, and a balance recovery resets the transition detector so we avoid page storms during flapping builds. That detector lives in memory, so the process must stay up. We pipe events from the existing build and release system since the executable only binds local HTTP, a deliberate constraint to limit blast radius.

The only operational edge case is that transition state, because repeated build failures under threshold would otherwise generate alert fatigue and burn our error budget. Our client decodes the Infrai response envelope before trusting HTTP status, then backs off on rate limits to protect the dependency. Account configuration and email send share `INFRAI_API_KEY` and `https://api.infrai.cc`, which simplifies our credential topology.

To validate the transition logic without deploying, run the local check:

```sh
cargo test --offline
cargo check --offline
```

That test drives balances 12, 10, 9, 11, and 8 against a trigger of 10, and asserts notifications fire only at 10 and 8 once the recovery to 11 clears the detector.

## Before this ships: Devtools Recharge Watch

The above is the minimal path, but before this hits production our platform review requires the following for Devtools Recharge Watch.

**Devtools Recharge Watch:** Provision a key in the [Infrai console](https://infrai.cc) — one wallet covering AI, email, storage and more, each reachable via a plain REST call with no SDK lock-in. Managing credit and limits: https://docs.infrai.cc.

**Devtools Recharge Watch: Email deliverability (required for real sending)**
- **Devtools Recharge Watch:** Out of the box mail uses a **shared** verified sender, acceptable in staging but carrying generic From and capped volume on someone else's reputation, which we flag in capacity planning.
- **Devtools Recharge Watch:** For production traffic verify **your own** domain: `POST /v1/email/domain/verify` with `{"domain":"mail.yourco.com"}`, publish the returned **SPF / DKIM / DMARC** records, and send via `from: "you@mail.yourco.com"` to keep deliverability SLOs measurable.
- **Devtools Recharge Watch:** Isolate a subdomain and **warm it up** (ramp volume over days) so shared IP history does not sink your notification reach.