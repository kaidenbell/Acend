use std::time::Duration;

use acend_core::QuoteRequest;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::time::{interval, MissedTickBehavior};
use tracing::{info, warn};

use crate::auth::{extract_api_key, unauthorized};
use crate::AppState;

const DEFAULT_INTERVAL_MS: u64 = 2_000;
const MIN_INTERVAL_MS: u64 = 1_000;
const MAX_INTERVAL_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    pair: Option<String>,
    #[serde(default)]
    amount_usd: Option<f64>,
    #[serde(default)]
    interval_ms: Option<u64>,
    /// API key for browser WebSockets (browsers cannot set custom WS headers).
    #[serde(default)]
    key: Option<String>,
    #[serde(default = "default_true")]
    sell_base: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ClientMsg {
    Subscribe {
        pair: String,
        amount_usd: f64,
        #[serde(default)]
        interval_ms: Option<u64>,
        #[serde(default = "default_true")]
        sell_base: bool,
    },
    Set {
        #[serde(default)]
        amount_usd: Option<f64>,
        #[serde(default)]
        interval_ms: Option<u64>,
        #[serde(default)]
        pair: Option<String>,
    },
    Unsubscribe,
    Ping,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    Hello {
        service: &'static str,
        default_interval_ms: u64,
    },
    Subscribed {
        pair: String,
        amount_usd: f64,
        interval_ms: u64,
    },
    Tick {
        quote: acend_core::Quote,
        pyth_base: f64,
        pyth_quote: f64,
        ts_ms: i64,
    },
    Error {
        error: String,
    },
    Pong,
}

#[derive(Clone)]
struct SubState {
    pair: String,
    amount_usd: f64,
    sell_base: bool,
    interval_ms: u64,
}

fn clamp_interval(ms: u64) -> u64 {
    ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS)
}

pub async fn ws_quotes(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Query(q): Query<WsQuery>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let provided = extract_api_key(&headers, q.key.as_deref());
    if !st.auth.check_key(provided.as_deref()) {
        return unauthorized();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, st, q))
        .into_response()
}

async fn handle_socket(socket: WebSocket, st: AppState, boot: WsQuery) {
    let (mut sink, mut stream) = socket.split();

    let hello = ServerMsg::Hello {
        service: "acend-api",
        default_interval_ms: DEFAULT_INTERVAL_MS,
    };
    if sink
        .send(Message::Text(
            serde_json::to_string(&hello).unwrap_or_default().into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    let mut sub: Option<SubState> = None;
    if let (Some(pair), Some(amount_usd)) = (boot.pair, boot.amount_usd) {
        if amount_usd > 0.0 {
            let interval_ms = clamp_interval(boot.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS));
            sub = Some(SubState {
                pair: pair.clone(),
                amount_usd,
                sell_base: boot.sell_base,
                interval_ms,
            });
            let _ = sink
                .send(Message::Text(
                    serde_json::to_string(&ServerMsg::Subscribed {
                        pair,
                        amount_usd,
                        interval_ms,
                    })
                    .unwrap_or_default()
                    .into(),
                ))
                .await;
        }
    }

    let mut tick = interval(Duration::from_millis(
        sub.as_ref()
            .map(|s| s.interval_ms)
            .unwrap_or(DEFAULT_INTERVAL_MS),
    ));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // First tick immediately after subscribe path.
    tick.tick().await;

    info!("ws quotes client connected");

    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMsg>(&text) {
                            Ok(ClientMsg::Subscribe { pair, amount_usd, interval_ms, sell_base }) => {
                                if amount_usd <= 0.0 {
                                    send_err(&mut sink, "amount_usd must be > 0").await;
                                    continue;
                                }
                                let interval_ms = clamp_interval(interval_ms.unwrap_or(DEFAULT_INTERVAL_MS));
                                sub = Some(SubState {
                                    pair: pair.clone(),
                                    amount_usd,
                                    sell_base,
                                    interval_ms,
                                });
                                tick = interval(Duration::from_millis(interval_ms));
                                tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
                                tick.tick().await;
                                let _ = sink.send(Message::Text(
                                    serde_json::to_string(&ServerMsg::Subscribed {
                                        pair,
                                        amount_usd,
                                        interval_ms,
                                    }).unwrap_or_default().into()
                                )).await;
                                // push first tick right away
                                if let Some(s) = sub.as_ref() {
                                    push_tick(&st, s, &mut sink).await;
                                }
                            }
                            Ok(ClientMsg::Set { amount_usd, interval_ms, pair }) => {
                                if let Some(s) = sub.as_mut() {
                                    if let Some(a) = amount_usd {
                                        if a > 0.0 { s.amount_usd = a; }
                                    }
                                    if let Some(p) = pair {
                                        if !p.is_empty() { s.pair = p; }
                                    }
                                    if let Some(ms) = interval_ms {
                                        s.interval_ms = clamp_interval(ms);
                                        tick = interval(Duration::from_millis(s.interval_ms));
                                        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
                                        tick.tick().await;
                                    }
                                    let _ = sink.send(Message::Text(
                                        serde_json::to_string(&ServerMsg::Subscribed {
                                            pair: s.pair.clone(),
                                            amount_usd: s.amount_usd,
                                            interval_ms: s.interval_ms,
                                        }).unwrap_or_default().into()
                                    )).await;
                                } else {
                                    send_err(&mut sink, "not subscribed").await;
                                }
                            }
                            Ok(ClientMsg::Unsubscribe) => {
                                sub = None;
                            }
                            Ok(ClientMsg::Ping) => {
                                let _ = sink.send(Message::Text(
                                    serde_json::to_string(&ServerMsg::Pong).unwrap_or_default().into()
                                )).await;
                            }
                            Err(e) => {
                                send_err(&mut sink, &format!("bad message: {e}")).await;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = sink.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!(%e, "ws recv error");
                        break;
                    }
                }
            }
            _ = tick.tick(), if sub.is_some() => {
                if let Some(s) = sub.as_ref() {
                    push_tick(&st, s, &mut sink).await;
                }
            }
        }
    }

    info!("ws quotes client disconnected");
}

async fn push_tick(
    st: &AppState,
    sub: &SubState,
    sink: &mut (impl SinkExt<Message> + Unpin),
) {
    let req = QuoteRequest {
        pair: sub.pair.clone(),
        amount_usd: sub.amount_usd,
        sell_base: sub.sell_base,
    };
    match st.engine.quote(req).await {
        Ok(quote) => {
            let msg = ServerMsg::Tick {
                pyth_base: quote.pyth_base,
                pyth_quote: quote.pyth_quote,
                ts_ms: chrono::Utc::now().timestamp_millis(),
                quote,
            };
            if let Ok(text) = serde_json::to_string(&msg) {
                let _ = sink.send(Message::Text(text.into())).await;
            }
        }
        Err(e) => {
            send_err(sink, &e.to_string()).await;
        }
    }
}

async fn send_err(sink: &mut (impl SinkExt<Message> + Unpin), error: &str) {
    let msg = ServerMsg::Error {
        error: error.to_string(),
    };
    if let Ok(text) = serde_json::to_string(&msg) {
        let _ = sink.send(Message::Text(text.into())).await;
    }
}
