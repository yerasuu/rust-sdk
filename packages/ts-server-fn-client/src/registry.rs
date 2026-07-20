//! Global provider registry, keyed by the *service crate's* Cargo package
//! name. Generated client fns look themselves up with
//! `env!("CARGO_PKG_NAME")` — the token is emitted into the service crate,
//! so each service gets its own key (and therefore its own domain) for free.
//!
//! One registry per linked copy of this crate: every crate in the final
//! binary must resolve to a single semver-compatible `ts-server-fn-client`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::provider::{HttpProvider, RequestProvider};

type Registry = RwLock<HashMap<String, Arc<dyn RequestProvider>>>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the provider for a service package. `pkg` is the
/// Cargo package name **verbatim** (dashes included, e.g. `"search-service"`).
pub fn set_provider(pkg: &str, provider: impl RequestProvider + 'static) {
    registry()
        .write()
        .expect("ts-server-fn-client registry poisoned")
        .insert(pkg.to_string(), Arc::new(provider));
}

/// Convenience: register the default [`HttpProvider`] for a base URL.
pub fn set_endpoint(pkg: &str, base_url: &str) {
    set_provider(pkg, HttpProvider::new(base_url));
}

/// Look up the provider for a package. Called by generated code.
///
/// Panics when nothing is registered: a missing provider is a startup wiring
/// bug, not a recoverable condition.
#[doc(hidden)]
pub fn provider_for(pkg: &str) -> Arc<dyn RequestProvider> {
    let map = registry()
        .read()
        .expect("ts-server-fn-client registry poisoned");
    if let Some(p) = map.get(pkg) {
        return p.clone();
    }
    let mut known: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
    known.sort_unstable();
    panic!(
        "ts-server-fn-client: no provider registered for package `{pkg}`. \
         Call `ts_server_fn_client::set_endpoint(\"{pkg}\", \"https://…\")` \
         (or `set_provider`) once at startup. Registered packages: {known:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_endpoint_then_provider_for_roundtrips_and_resets() {
        set_endpoint("reg-test-pkg", "http://one.example.com/");
        let _p = provider_for("reg-test-pkg"); // must not panic
        // re-set is allowed (tests swap providers)
        set_endpoint("reg-test-pkg", "http://two.example.com");
        let _p = provider_for("reg-test-pkg");
    }

    #[test]
    #[should_panic(expected = "no provider registered for package `missing-pkg`")]
    fn provider_for_missing_panics_with_hint() {
        provider_for("missing-pkg");
    }
}
