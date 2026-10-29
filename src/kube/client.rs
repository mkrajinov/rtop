//! The async side: one [`State`] per runtime, answering [`Request`]s with
//! kube-rs calls. Handlers for watches, exec streams, debug containers and
//! the node helper are added story by story; until then they answer
//! `Unsupported`.

use std::sync::mpsc;

use kube::Client;
use kube::config::{Config, KubeConfigOptions, Kubeconfig};
use tokio::sync::mpsc::UnboundedReceiver;

use super::msg::{ErrorCode, Incoming, KubeError, Reply, Request};

pub struct State {
    out: mpsc::Sender<Incoming>,
    client: Option<Client>,
    context: String,
    namespace: String,
}

impl State {
    pub fn new(out: mpsc::Sender<Incoming>, preset: Option<(Client, String)>) -> Self {
        let (client, context) = match preset {
            Some((c, ctx)) => (Some(c), ctx),
            None => (None, String::new()),
        };
        let namespace = client.as_ref().map_or_else(String::new, |c| c.default_namespace().to_string());
        State { out, client, context, namespace }
    }

    /// Answer requests until `Shutdown` or until the UI side is gone.
    pub async fn serve(mut self, rx: &mut UnboundedReceiver<(u64, Request)>) {
        while let Some((id, req)) = rx.recv().await {
            let stop = req == Request::Shutdown;
            let reply = self.handle(req).await;
            if self.out.send(Incoming::Reply(id, reply)).is_err() || stop {
                return;
            }
        }
    }

    async fn handle(&mut self, req: Request) -> Reply {
        let result = match req {
            Request::Hello { context } => self.hello(context).await,
            Request::Shutdown => Ok(Reply::Done),
            other => Err(KubeError::new(ErrorCode::Unsupported, format!("not implemented yet: {other:?}"))),
        };
        result.unwrap_or_else(Reply::Err)
    }

    async fn hello(&mut self, context: Option<String>) -> Result<Reply, KubeError> {
        if self.client.is_none() {
            let (client, ctx, ns) = connect(context).await?;
            self.client = Some(client);
            self.context = ctx;
            self.namespace = ns;
        }
        let info = self.client()?.apiserver_version().await.map_err(map_err)?;
        Ok(Reply::Hello {
            context: self.context.clone(),
            namespace: self.namespace.clone(),
            server_version: info.git_version,
        })
    }

    fn client(&self) -> Result<&Client, KubeError> {
        self.client.as_ref().ok_or_else(|| KubeError::new(ErrorCode::Internal, "no client: send Hello first"))
    }
}

/// Read the kubeconfig the way kubectl does ($KUBECONFIG or ~/.kube/config,
/// exec credential plugins included) and build a client for one context.
async fn connect(context: Option<String>) -> Result<(Client, String, String), KubeError> {
    let invalid = |e: &dyn std::fmt::Display| KubeError::new(ErrorCode::Invalid, format!("kubeconfig: {e}"));
    let kubeconfig = Kubeconfig::read().map_err(|e| invalid(&e))?;
    let context = context
        .or_else(|| kubeconfig.current_context.clone())
        .ok_or_else(|| KubeError::new(ErrorCode::Invalid, "kubeconfig has no current context; pass --context"))?;
    if !kubeconfig.contexts.iter().any(|c| c.name == context) {
        return Err(KubeError::new(ErrorCode::Invalid, format!("context {context}: not in the kubeconfig")));
    }
    let options = KubeConfigOptions { context: Some(context.clone()), ..Default::default() };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .map_err(|e| KubeError::new(ErrorCode::Unauthorized, format!("credentials for {context}: {e}")))?;
    let namespace = config.default_namespace.clone();
    let client = Client::try_from(config).map_err(map_err)?;
    Ok((client, context, namespace))
}

/// Turn a kube-rs error into the codes the UI explains (FR-012).
pub fn map_err(e: kube::Error) -> KubeError {
    match e {
        kube::Error::Api(status) => {
            let code = match status.code {
                401 => ErrorCode::Unauthorized,
                403 if is_admission(&status.message) => ErrorCode::Admission,
                403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                400 | 422 if is_admission(&status.message) => ErrorCode::Admission,
                422 => ErrorCode::Invalid,
                408 | 504 => ErrorCode::Timeout,
                _ => ErrorCode::Internal,
            };
            let mut err = KubeError::new(code, status.message.clone());
            if code == ErrorCode::Forbidden {
                // "pods \"x\" is forbidden: User \"u\" cannot create resource
                // \"pods/exec\" in API group \"\" in the namespace \"ns\""
                err.verb = between(&status.message, "cannot ", " resource").map(str::to_string);
                err.resource = between(&status.message, "resource \"", "\"").map(str::to_string);
                err.namespace = between(&status.message, "in the namespace \"", "\"").map(str::to_string);
            }
            err
        }
        kube::Error::Auth(e) => KubeError::new(ErrorCode::Unauthorized, format!("credentials: {e}")),
        e @ (kube::Error::HyperError(_) | kube::Error::Service(_) | kube::Error::RustlsTls(_)) => {
            KubeError::new(ErrorCode::Unreachable, format!("API server unreachable: {e}"))
        }
        e => KubeError::new(ErrorCode::Internal, e.to_string()),
    }
}

fn is_admission(message: &str) -> bool {
    message.contains("violates PodSecurity") || message.contains("admission webhook")
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = &s[s.find(start)? + start.len()..];
    Some(&rest[..rest.find(end)?])
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use http::{Request as HttpRequest, Response};
    use kube::client::Body;
    use tower_test::mock;

    use super::super::KubeHandle;
    use super::super::msg::{ErrorCode, Incoming, Reply, Request};

    pub type Handle = mock::Handle<HttpRequest<Body>, Response<Body>>;

    /// A kube thread whose client talks to a scripted API server.
    pub fn mocked() -> (KubeHandle, Handle) {
        let (service, handle) = mock::pair::<HttpRequest<Body>, Response<Body>>();
        (KubeHandle::with_client(move || kube::Client::new(service, "rtop-demo"), "kind-rtop"), handle)
    }

    /// Answer the next API call with `status` and a JSON body; returns the
    /// request path so tests can check what was asked.
    pub async fn answer(handle: &mut Handle, status: u16, body: serde_json::Value) -> String {
        let (req, send) = handle.next_request().await.expect("an API request");
        let path = req.uri().path().to_string();
        let resp = Response::builder().status(status).body(Body::from(body.to_string().into_bytes())).unwrap();
        send.send_response(resp);
        path
    }

    pub fn reply(k: &KubeHandle, id: u64) -> Reply {
        match k.recv_timeout(Duration::from_secs(5)) {
            Some(Incoming::Reply(got, r)) if got == id => r,
            other => panic!("expected reply {id}, got {other:?}"),
        }
    }

    fn status(code: u16, reason: &str, message: &str) -> serde_json::Value {
        serde_json::json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                           "code": code, "reason": reason, "message": message})
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn hello_reports_context_namespace_and_version() {
        let (mut k, mut api) = mocked();
        let id = k.send(Request::Hello { context: None });
        let path = rt().block_on(answer(
            &mut api,
            200,
            serde_json::json!({
                "major": "1", "minor": "33", "gitVersion": "v1.33.1", "gitCommit": "", "gitTreeState": "",
                "buildDate": "", "goVersion": "", "compiler": "", "platform": "linux/arm64"
            }),
        ));
        assert_eq!(path, "/version");
        assert_eq!(
            reply(&k, id),
            Reply::Hello {
                context: "kind-rtop".into(),
                namespace: "rtop-demo".into(),
                server_version: "v1.33.1".into()
            }
        );
    }

    #[test]
    fn api_errors_map_to_codes() {
        let forbidden = "pods \"multi\" is forbidden: User \"dev\" cannot create resource \"pods/exec\" in API group \"\" in the namespace \"rtop-demo\"";
        let cases = [
            (401, "Unauthorized", "Unauthorized", ErrorCode::Unauthorized),
            (403, "Forbidden", forbidden, ErrorCode::Forbidden),
            (404, "NotFound", "not found", ErrorCode::NotFound),
            (
                403,
                "Forbidden",
                "pods \"x\" is forbidden: violates PodSecurity \"baseline:latest\": host namespaces",
                ErrorCode::Admission,
            ),
        ];
        for (code, reason, message, want) in cases {
            let (mut k, mut api) = mocked();
            let id = k.send(Request::Hello { context: None });
            rt().block_on(answer(&mut api, code, status(code, reason, message)));
            let Reply::Err(e) = reply(&k, id) else { panic!("{code} should fail") };
            assert_eq!(e.code, want, "{code} {message}");
            if want == ErrorCode::Forbidden {
                assert_eq!(
                    e.to_string(),
                    "forbidden: create pods/exec in rtop-demo (see the README for the permissions each mode needs)"
                );
            }
        }
    }

    #[test]
    fn unknown_requests_are_unsupported_not_fatal() {
        let (mut k, _api) = mocked();
        let id = k.send(Request::ListNodes);
        let Reply::Err(e) = reply(&k, id) else { panic!() };
        assert_eq!(e.code, ErrorCode::Unsupported);
        // The thread still answers afterwards.
        let id = k.send(Request::Shutdown);
        assert_eq!(reply(&k, id), Reply::Done);
    }
}
