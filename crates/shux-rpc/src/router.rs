//! Method router — dispatches JSON-RPC requests to handler functions.
//!
//! The router maintains a HashMap of method names to handler trait objects.
//! New methods are registered via `router.register("method.name", handler)`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tracing::{debug, warn};

use crate::error::RpcError;
use crate::policy::Policy;

/// The result type for RPC method handlers.
pub type HandlerResult = Result<Value, RpcError>;

/// A boxed future that resolves to a HandlerResult.
pub type HandlerFuture = Pin<Box<dyn Future<Output = HandlerResult> + Send>>;

/// Trait for RPC method handlers.
///
/// Handlers receive the `params` field from the JSON-RPC request (may be null)
/// and return either a successful result value or an RPC error.
pub trait Handler: Send + Sync + 'static {
    /// Handle a JSON-RPC request.
    fn handle(&self, params: Option<Value>) -> HandlerFuture;
}

/// Implement Handler for async functions.
///
/// This allows registering closures and functions directly:
/// ```ignore
/// router.register("system.version", |_params| async {
///     Ok(serde_json::json!({"version": "0.1.0"}))
/// });
/// ```
impl<F, Fut> Handler for F
where
    F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = HandlerResult> + Send + 'static,
{
    fn handle(&self, params: Option<Value>) -> HandlerFuture {
        Box::pin((self)(params))
    }
}

/// The method router.
///
/// Thread-safe (uses Arc internally for handler storage).
/// Cloning is cheap.
#[derive(Clone)]
pub struct Router {
    methods: Arc<HashMap<String, Arc<dyn Handler>>>,
    policies: Arc<HashMap<String, Policy>>,
    owner_local: Arc<HashSet<String>>,
    accepts: Arc<HashMap<String, Accepts>>,
}

/// The top-level parameters a method accepts. Any other key is refused before
/// the handler runs, so a newer client's field never reaches a daemon that
/// would ignore it.
pub type Accepts = &'static [&'static str];

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Router {
            methods: Arc::new(HashMap::new()),
            policies: Arc::new(HashMap::new()),
            owner_local: Arc::default(),
            accepts: Arc::default(),
        }
    }

    /// Create a router builder for registering methods.
    pub fn builder() -> RouterBuilder {
        RouterBuilder {
            methods: HashMap::new(),
            policies: HashMap::new(),
            owner_local: HashSet::new(),
            accepts: HashMap::new(),
        }
    }

    /// Whether `method` answers only a request on the daemon's own socket.
    pub fn is_owner_local(&self, method: &str) -> bool {
        self.owner_local.contains(method)
    }

    /// Dispatch a JSON-RPC request to the appropriate handler.
    ///
    /// Returns `Err(RpcError)` if the method is not found, or is owner-local
    /// and this request did not come over the daemon's own socket.
    pub async fn dispatch(&self, method: &str, params: Option<Value>) -> HandlerResult {
        if self.is_owner_local(method) && crate::current_transport() != Some(crate::Transport::Uds)
        {
            warn!(method, "owner-local method refused off the daemon socket");
            return Err(RpcError::with_data(
                crate::ErrorCode::PermissionDenied,
                serde_json::json!({
                    "method": method,
                    "reason": "owner_local",
                    "hint": "only the daemon's own socket may call this; run the shux CLI as its user",
                }),
            ));
        }
        match self.methods.get(method) {
            Some(handler) => {
                let mut params = params;
                if let Some(accepted) = self.accepts.get(method) {
                    check_params(method, accepted, params.as_ref())?;
                    // An unknown key set to `null` asks for nothing: gone, so
                    // a handler that parses strictly sees an absent key.
                    if let Some(Value::Object(object)) = params.as_mut() {
                        object.retain(|k, v| accepted.contains(&k.as_str()) || !v.is_null());
                    }
                }
                debug!(method, "dispatching RPC method");
                handler.handle(params).await
            }
            None => {
                warn!(method, "method not found");
                Err(RpcError::method_not_found(method))
            }
        }
    }

    /// List all registered method names.
    pub fn methods(&self) -> Vec<&str> {
        self.methods.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a method is registered.
    pub fn has_method(&self, method: &str) -> bool {
        self.methods.contains_key(method)
    }

    /// Per-method sensitivity policy, if declared. Consumed by the
    /// plugin permission enforcer; direct CLI clients ignore this.
    pub fn policy(&self, method: &str) -> Option<&Policy> {
        self.policies.get(method)
    }

    /// Panic on boot if any registered method has no declared parameters.
    pub fn assert_every_route_declares_params(&self) {
        let mut missing: Vec<&str> = self
            .methods
            .keys()
            .filter(|m| !self.accepts.contains_key(*m))
            .map(|s| s.as_str())
            .collect();
        missing.sort_unstable();
        if !missing.is_empty() {
            panic!("RPC method(s) registered without declared parameters: {missing:?}");
        }
    }

    /// Panic on boot if any registered handler has no declared policy.
    /// Catches "added a new RPC, forgot to classify its sensitivity"
    /// at startup rather than letting a fresh method silently inherit
    /// permissive defaults.
    pub fn assert_every_route_has_policy(&self) {
        let missing: Vec<&str> = self
            .methods
            .keys()
            .filter(|m| !self.policies.contains_key(*m))
            .map(|s| s.as_str())
            .collect();
        if !missing.is_empty() {
            panic!(
                "RPC method(s) registered without a sensitivity policy: {missing:?}. \
                 Use `register_with_policy(...)` or document a deliberate \
                 `Sensitivity::Public` classification."
            );
        }
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("method_count", &self.methods.len())
            .field("methods", &self.methods())
            .finish()
    }
}

/// Builder for constructing a Router.
///
/// Methods are registered on the builder, then `.build()` produces
/// an immutable Router.
pub struct RouterBuilder {
    methods: HashMap<String, Arc<dyn Handler>>,
    policies: HashMap<String, Policy>,
    owner_local: HashSet<String>,
    accepts: HashMap<String, Accepts>,
}

impl RouterBuilder {
    /// Register a method handler. Use this for routes that don't
    /// participate in the plugin permission model (test-only routes,
    /// development scaffolding). For production routes, prefer
    /// [`Self::register_with_policy`] so the startup assertion can
    /// catch missing classifications.
    pub fn register(mut self, method: impl Into<String>, handler: impl Handler) -> Self {
        self.methods.insert(method.into(), Arc::new(handler));
        self
    }

    /// Register a method handler with an explicit sensitivity policy.
    /// The plugin dispatcher consults this policy on every plugin
    /// RPC frame to decide allow/deny. CLI clients bypass policy
    /// checks (they are the user).
    pub fn register_with_policy(
        mut self,
        method: impl Into<String>,
        policy: Policy,
        handler: impl Handler,
    ) -> Self {
        let m: String = method.into();
        self.policies.insert(m.clone(), policy);
        self.methods.insert(m, Arc::new(handler));
        self
    }

    /// [`Self::register_with_policy`] for a method that changes state in
    /// several steps: each call runs to completion even if its caller hangs
    /// up (see [`crate::run_to_completion`]). Not for a method that waits, or
    /// one that reads its connection's scope.
    pub fn register_to_completion(
        self,
        method: impl Into<String>,
        policy: Policy,
        handler: impl Handler,
    ) -> Self {
        self.register_with_policy(method, policy, move |params: Option<Value>| {
            crate::run_to_completion(handler.handle(params))
        })
    }

    /// Accept `method` only over the daemon's own socket ([`crate::Transport::Uds`]):
    /// never over TCP, from a plugin, or from a call that names no transport.
    pub fn owner_local(mut self, method: impl Into<String>) -> Self {
        self.owner_local.insert(method.into());
        self
    }

    /// Declare the top-level parameters each named method accepts.
    pub fn accepts(mut self, table: &[(&str, Accepts)]) -> Self {
        for (method, keys) in table {
            self.accepts.insert((*method).to_string(), keys);
        }
        self
    }

    /// Build the router.
    pub fn build(self) -> Router {
        Router {
            methods: Arc::new(self.methods),
            policies: Arc::new(self.policies),
            owner_local: Arc::new(self.owner_local),
            accepts: Arc::new(self.accepts),
        }
    }
}

/// `params` is absent, `null` or an object holding only `accepted` keys.
fn check_params(method: &str, accepted: Accepts, params: Option<&Value>) -> Result<(), RpcError> {
    let object = match params {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Object(object)) => object,
        Some(_) => {
            return Err(RpcError::with_data(
                crate::ErrorCode::InvalidParams,
                serde_json::json!({
                    "kind": "params_not_object",
                    "method": method,
                    "detail": format!("{method} takes its parameters as a JSON object"),
                }),
            ));
        }
    };
    // An unknown key set to `null` asks for nothing, as if it were absent.
    let mut unknown: Vec<&str> = object
        .iter()
        .filter(|(k, v)| !accepted.contains(&k.as_str()) && !v.is_null())
        .map(|(k, _)| k.as_str())
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let mut known = accepted.to_vec();
    known.sort_unstable();
    Err(RpcError::with_data(
        crate::ErrorCode::InvalidParams,
        serde_json::json!({
            "kind": UNKNOWN_PARAMS,
            "method": method,
            "params": unknown,
            "accepted": known,
            "detail": format!("{method} does not take {}", unknown.join(", ")),
        }),
    ))
}

/// `error.data.kind` of a call naming a parameter its method does not take.
pub const UNKNOWN_PARAMS: &str = "unknown_params";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatch_registered_method() {
        let router = Router::builder()
            .register("system.version", |_params: Option<Value>| async {
                Ok(serde_json::json!({"version": "0.1.0"}))
            })
            .build();

        let result = router.dispatch("system.version", None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["version"], "0.1.0");
    }

    #[tokio::test]
    async fn test_dispatch_unknown_method() {
        let router = Router::builder().build();

        let result = router.dispatch("nonexistent.method", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, -32601);
    }

    #[tokio::test]
    async fn test_dispatch_with_params() {
        let router = Router::builder()
            .register("echo", |params: Option<Value>| async move {
                Ok(params.unwrap_or(Value::Null))
            })
            .build();

        let params = serde_json::json!({"name": "shux"});
        let result = router.dispatch("echo", Some(params.clone())).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), params);
    }

    #[test]
    fn test_router_methods_list() {
        let router = Router::builder()
            .register("system.version", |_: Option<Value>| async {
                Ok(Value::Null)
            })
            .register("system.health", |_: Option<Value>| async {
                Ok(Value::Null)
            })
            .build();

        let methods = router.methods();
        assert_eq!(methods.len(), 2);
        assert!(router.has_method("system.version"));
        assert!(router.has_method("system.health"));
        assert!(!router.has_method("nonexistent"));
    }

    fn counting_router() -> (Router, Arc<std::sync::atomic::AtomicUsize>) {
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = ran.clone();
        let router = Router::builder()
            .register("m.one", move |_: Option<Value>| {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(Value::Null) }
            })
            .accepts(&[("m.one", &["a", "b"])])
            .build();
        (router, ran)
    }

    /// A key the method does not take is refused before its handler runs,
    /// naming the key: an older daemon never silently ignores a newer field.
    #[tokio::test]
    async fn an_undeclared_parameter_is_refused_before_the_handler_runs() {
        let (router, ran) = counting_router();
        let err = router
            .dispatch("m.one", Some(serde_json::json!({"a": 1, "zz": 2, "c": 3})))
            .await
            .unwrap_err();
        assert_eq!(err.code, -32602);
        let data = err.data.unwrap();
        assert_eq!(data["kind"], UNKNOWN_PARAMS);
        assert_eq!(data["params"], serde_json::json!(["c", "zz"]));
        assert_eq!(data["accepted"], serde_json::json!(["a", "b"]));
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Declared keys, no params and `null` params all reach the handler; a
    /// non-object is refused.
    #[tokio::test]
    async fn declared_absent_and_null_params_pass_and_a_non_object_does_not() {
        let (router, ran) = counting_router();
        for ok in [
            None,
            Some(Value::Null),
            Some(serde_json::json!({"a": 1, "b": null})),
            Some(serde_json::json!({"a": 1, "zz": null})),
        ] {
            router.dispatch("m.one", ok).await.unwrap();
        }
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 4);
        let err = router
            .dispatch("m.one", Some(serde_json::json!(["a"])))
            .await
            .unwrap_err();
        assert_eq!(err.data.unwrap()["kind"], "params_not_object");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    /// An unknown key set to `null` never reaches the handler: one that
    /// parses strictly itself sees exactly what an absent key gives it.
    #[tokio::test]
    async fn an_unknown_null_key_is_removed_before_the_handler() {
        let router = Router::builder()
            .register("m.echo", |p: Option<Value>| async move {
                Ok(p.unwrap_or(Value::Null))
            })
            .accepts(&[("m.echo", &["a", "b"])])
            .build();
        let seen = router
            .dispatch(
                "m.echo",
                Some(serde_json::json!({"a": 1, "b": null, "zz": null})),
            )
            .await
            .unwrap();
        assert_eq!(seen, serde_json::json!({"a": 1, "b": null}));
    }

    #[test]
    #[should_panic(expected = "without declared parameters")]
    fn a_route_without_declared_params_fails_the_boot_check() {
        Router::builder()
            .register("m.undeclared", |_: Option<Value>| async { Ok(Value::Null) })
            .build()
            .assert_every_route_declares_params();
    }

    #[test]
    fn test_router_is_clone() {
        let router = Router::builder()
            .register("test", |_: Option<Value>| async { Ok(Value::Null) })
            .build();

        let cloned = router.clone();
        assert!(cloned.has_method("test"));
    }
}
