//! Method router — dispatches JSON-RPC requests to handler functions.
//!
//! The router maintains a HashMap of method names to handler trait objects.
//! New methods are registered via `router.register("method.name", handler)`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tracing::{debug, warn};

use crate::error::RpcError;

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
}

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Router {
            methods: Arc::new(HashMap::new()),
        }
    }

    /// Create a router builder for registering methods.
    pub fn builder() -> RouterBuilder {
        RouterBuilder {
            methods: HashMap::new(),
        }
    }

    /// Dispatch a JSON-RPC request to the appropriate handler.
    ///
    /// Returns `Err(RpcError)` if the method is not found.
    pub async fn dispatch(&self, method: &str, params: Option<Value>) -> HandlerResult {
        match self.methods.get(method) {
            Some(handler) => {
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
}

impl RouterBuilder {
    /// Register a method handler.
    pub fn register(mut self, method: impl Into<String>, handler: impl Handler) -> Self {
        self.methods.insert(method.into(), Arc::new(handler));
        self
    }

    /// Build the router.
    pub fn build(self) -> Router {
        Router {
            methods: Arc::new(self.methods),
        }
    }
}

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

    #[test]
    fn test_router_is_clone() {
        let router = Router::builder()
            .register("test", |_: Option<Value>| async { Ok(Value::Null) })
            .build();

        let cloned = router.clone();
        assert!(cloned.has_method("test"));
    }
}
