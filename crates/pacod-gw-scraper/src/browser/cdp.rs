//! A small client of the Chrome DevTools Protocol: commands with their answers, and events by
//! session, over one WebSocket to the browser. The scraper needs a dozen of its methods.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// How long a command may wait for its answer.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type Listeners = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>>;

/// One connection to the browser.
pub struct Cdp {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    listeners: Listeners,
    next: AtomicU64,
    open: Arc<std::sync::atomic::AtomicBool>,
}

/// Where the browser's DevTools are: its address as an IP and port (the browser refuses a Host
/// header that is not an IP address or `localhost`), and the WebSocket of the browser target.
pub struct Endpoint {
    pub addr: SocketAddr,
    pub product: String,
    pub user_agent: String,
    pub ws_path: String,
}

/// Asks the browser at `base` (`http://gw-browser:9222`) where its DevTools are.
pub async fn endpoint(base: &str) -> anyhow::Result<Endpoint> {
    let url = url::Url::parse(base)?;
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("{base} has no host"))?;
    let port = url.port_or_known_default().unwrap_or(9222);
    let addr = tokio::net::lookup_host((host, port))
        .await?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| anyhow::anyhow!("{host} does not resolve"))?;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let version: Value =
        http.get(format!("http://{addr}/json/version")).send().await?.error_for_status()?.json().await?;
    let ws = version["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("the browser names no DevTools socket"))?;
    let ws_path = url::Url::parse(ws)?.path().to_owned();
    Ok(Endpoint {
        addr,
        product: version["Browser"].as_str().unwrap_or_default().to_owned(),
        user_agent: version["User-Agent"].as_str().unwrap_or_default().to_owned(),
        ws_path,
    })
}

impl Cdp {
    /// Opens the browser's DevTools socket.
    pub async fn connect(endpoint: &Endpoint) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(endpoint.addr).await?;
        let request = format!("ws://{}{}", endpoint.addr, endpoint.ws_path).into_client_request()?;
        let (socket, _) = tokio_tungstenite::client_async(request, stream).await?;
        let (mut sink, mut source) = socket.split();
        let (out, mut outgoing) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::default();
        let listeners: Listeners = Arc::default();
        let open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        tokio::spawn(async move {
            while let Some(m) = outgoing.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        let (p, l, o) = (pending.clone(), listeners.clone(), open.clone());
        tokio::spawn(async move {
            while let Some(Ok(message)) = source.next().await {
                let Message::Text(text) = message else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = v["id"].as_u64() {
                    if let Some(answer) = p.lock().unwrap().remove(&id) {
                        let result = match v.get("error") {
                            Some(e) => {
                                Err(e["message"].as_str().unwrap_or("the browser refused the command").to_owned())
                            }
                            None => Ok(v["result"].clone()),
                        };
                        let _ = answer.send(result);
                    }
                } else if let Some(session) = v["sessionId"].as_str()
                    && let Some(listener) = l.lock().unwrap().get(session)
                {
                    let _ = listener.send(v);
                }
            }
            // The socket closed: whoever waits learns it now, not after a timeout.
            o.store(false, Ordering::Relaxed);
            p.lock().unwrap().clear();
            l.lock().unwrap().clear();
        });
        Ok(Self { out, pending, listeners, next: AtomicU64::new(1), open })
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// Sends a command, to the browser or to a page's session, and waits for its answer.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> anyhow::Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            message["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.out.send(Message::Text(message.to_string().into())).is_err() {
            anyhow::bail!("the connection to the browser is closed");
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => anyhow::bail!("{method}: {e}"),
            Ok(Err(_)) => anyhow::bail!("{method}: the connection to the browser closed"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                anyhow::bail!("{method}: the browser did not answer within {} s", COMMAND_TIMEOUT.as_secs())
            }
        }
    }

    /// The events of a page's session, from now on.
    pub fn events(&self, session: &str) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.listeners.lock().unwrap().insert(session.to_owned(), tx);
        rx
    }

    pub fn forget(&self, session: &str) {
        self.listeners.lock().unwrap().remove(session);
    }
}
