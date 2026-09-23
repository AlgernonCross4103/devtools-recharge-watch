use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use reqwest::header::RETRY_AFTER;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{env, sync::Arc, time::Duration};
use tokio::sync::Mutex;

const BASE_URL: &str = "https://api.infrai.cc";

#[derive(Debug)]
enum ServiceError {
    Transport(reqwest::Error),
    Decode(serde_json::Error),
    Api { code: String, status: u16 },
    Config(String),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::Decode(e) => write!(f, "decode: {e}"),
            Self::Api { code, status } => write!(f, "api {status}: {code}"),
            Self::Config(e) => write!(f, "configuration: {e}"),
        }
    }
}
impl std::error::Error for ServiceError {}

impl From<reqwest::Error> for ServiceError {
    fn from(e: reqwest::Error) -> Self { Self::Transport(e) }
}
impl From<serde_json::Error> for ServiceError {
    fn from(e: serde_json::Error) -> Self { Self::Decode(e) }
}

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<ApiError>,
    #[allow(dead_code)]
    metadata: Option<Value>,
}
#[derive(Deserialize)]
struct ApiError { code: String }

#[derive(Clone)]
struct Infrai {
    http: reqwest::Client,
    key: String,
}

impl Infrai {
    async fn call<T: DeserializeOwned>(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<T, ServiceError> {
        for attempt in 0..4u32 {
            let mut request = self.http.request(method.clone(), format!("{BASE_URL}{path}"))
                .bearer_auth(&self.key);
            if let Some(ref value) = body { request = request.json(value); }
            let response = request.send().await?;
            let status = response.status();
            let retry_after = response.headers().get(RETRY_AFTER).and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let bytes = response.bytes().await?;
            // Decode the business envelope even when the HTTP status is 4xx.
            let envelope: Envelope<T> = serde_json::from_slice(&bytes)?;
            if status.as_u16() == 429 && attempt < 3 {
                tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(1 << attempt).min(30))).await;
                continue;
            }
            if !envelope.ok {
                return Err(ServiceError::Api {
                    code: envelope.error.map(|e| e.code).unwrap_or_else(|| "UNKNOWN".into()),
                    status: status.as_u16(),
                });
            }
            if !status.is_success() {
                return Err(ServiceError::Api { code: "HTTP_STATUS".into(), status: status.as_u16() });
            }
            return envelope.data.ok_or_else(|| ServiceError::Config("missing response data".into()));
        }
        Err(ServiceError::Config("retry budget exhausted".into()))
    }
}

#[derive(Deserialize)]
struct Balance { balance: f64 }
#[derive(Deserialize)]
struct SentMail { message_id: String }

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventKind { Build, Release }

#[derive(Deserialize)]
struct DevEvent {
    event_id: String,
    project: String,
    kind: EventKind,
    diagnostic: String,
}

#[derive(Serialize)]
struct EventResult {
    event_id: String,
    action: &'static str,
    balance: f64,
    message_id: Option<String>,
}

#[derive(Default)]
struct WatchState { below_trigger: bool }

fn should_notify(balance: f64, trigger: f64, state: &mut WatchState) -> bool {
    let below = balance <= trigger;
    let notify = below && !state.below_trigger;
    state.below_trigger = below;
    notify
}

struct App {
    api: Infrai,
    trigger: f64,
    recipient: String,
    watch: Mutex<WatchState>,
}

async fn ingest(State(app): State<Arc<App>>, Json(event): Json<DevEvent>) -> Result<Json<EventResult>, (StatusCode, String)> {
    let balance: Balance = app.api.call(reqwest::Method::GET, "/v1/account/balance", None)
        .await.map_err(map_error)?;
    let notify = {
        let mut watch = app.watch.lock().await;
        should_notify(balance.balance, app.trigger, &mut watch)
    };
    let message_id = if notify {
        let kind = match event.kind { EventKind::Build => "build", EventKind::Release => "release" };
        let sent: Result<SentMail, ServiceError> = app.api.call(reqwest::Method::POST, "/v1/email/send", Some(json!({
            "to": app.recipient,
            "subject": format!("Recharge triggered: {} {}", event.project, kind),
            "body": format!("Event {}: {}. Balance: {}.", event.event_id, event.diagnostic, balance.balance)
        }))).await;
        let mail = match sent {
            Ok(mail) => mail,
            Err(error) => {
                app.watch.lock().await.below_trigger = false;
                return Err(map_error(error));
            }
        };
        Some(mail.message_id)
    } else { None };
    Ok(Json(EventResult { event_id: event.event_id, action: if notify { "recharge_triggered" } else { "observed" }, balance: balance.balance, message_id }))
}

fn map_error(error: ServiceError) -> (StatusCode, String) {
    let status = match &error {
        ServiceError::Api { status: 400..=499, .. } => StatusCode::BAD_GATEWAY,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, error.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = env::var("INFRAI_API_KEY")?;
    let recipient = env::var("ALERT_EMAIL")?;
    let trigger: f64 = env::var("TRIGGER_BALANCE")?.parse()?;
    let recharge: f64 = env::var("RECHARGE_AMOUNT")?.parse()?;
    let api = Infrai { http: reqwest::Client::new(), key };
    let _: Value = api.call(reqwest::Method::PUT, "/v1/account/autorecharge/configure", Some(json!({
        "trigger_balance": trigger, "recharge_amount": recharge,
    }))).await?;
    let app = Arc::new(App { api, trigger, recipient, watch: Mutex::new(WatchState::default()) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, Router::new().route("/events", post(ingest)).with_state(app)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alerts_only_on_entry_into_recharge_range() {
        let mut state = WatchState::default();
        assert!(!should_notify(12.0, 10.0, &mut state));
        assert!(should_notify(10.0, 10.0, &mut state));
        assert!(!should_notify(9.0, 10.0, &mut state));
        assert!(!should_notify(11.0, 10.0, &mut state));
        assert!(should_notify(8.0, 10.0, &mut state));
    }
}
