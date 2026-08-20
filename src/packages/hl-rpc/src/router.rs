//! How a library exposes its routes, and how an application merges several
//! libraries onto one socket.
//!
//! Transport-agnostic on purpose. The daemon API is the base API and our own
//! routes sit alongside it, so in one composition these routes are reached over
//! HTTP on a unix socket and in another they are called directly. Nothing here
//! knows which: a [`Call`] is a method, a path, arguments and bytes, and an
//! [`Answer`] is an outcome and bytes.
//!
//! Every route names the capability it requires, and [`Routes::dispatch`]
//! checks it before the handler runs. The handler receives a [`Permit`], which
//! only [`Authority`] can produce, so a route cannot be served without its
//! check having happened.

use std::collections::BTreeMap;

use crate::authority::{Authority, Denial, Permit};
use crate::capability::CapabilityKey;

/// The request methods a route can be reached by.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
}

/// One call to a route.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Call {
    /// Path segments captured by the pattern, such as the container name in
    /// `/containers/{id}/start`.
    pub arguments: BTreeMap<String, String>,
    /// Arguments carried beside the path, however the transport spells them.
    pub query: BTreeMap<String, String>,
    /// The request body, already read.
    pub body: Vec<u8>,
}

/// What became of a call.
///
/// Deliberately not a status code: an application binding an HTTP socket maps
/// these onto one, and an application calling a route directly does not have to
/// invent one to read the answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Done,
    Created,
    Absent,
    Conflict,
    Denied,
    Failed,
}

/// The answer to a call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    pub outcome: Outcome,
    pub body: Vec<u8>,
}

impl Answer {
    /// A successful answer carrying a body.
    #[must_use]
    pub const fn done(body: Vec<u8>) -> Self {
        Self {
            outcome: Outcome::Done,
            body,
        }
    }

    /// A refusal, described so the caller learns it may not rather than that
    /// there is nothing there.
    #[must_use]
    pub fn denied(denial: &Denial) -> Self {
        Self {
            outcome: Outcome::Denied,
            body: denial.to_string().into_bytes(),
        }
    }
}

/// One route a library exposes.
#[async_trait::async_trait]
pub trait Route: Send + Sync {
    /// How the route is reached.
    fn method(&self) -> Method;

    /// The path it answers on, with `{name}` for a captured segment. There is
    /// no mount prefix: a library declares the whole path it occupies.
    fn path(&self) -> &'static str;

    /// The capability a caller must hold. Only a declared capability can name
    /// itself here, so a route cannot require something nothing granted.
    fn requirement(&self) -> CapabilityKey;

    /// Serves the call. The permit is proof [`Self::requirement`] was checked,
    /// and derefs to the [`Authority`] a handler needs for any further check,
    /// such as confining a path or reaching a second port.
    async fn call(&self, call: &Call, permit: Permit<'_, Authority>) -> Answer;
}

/// A library's routes.
pub trait Router {
    /// Everything this library exposes. Implemented by a domain crate; the
    /// application that binds a socket merges these with other routers'.
    fn routes(&self) -> Vec<Box<dyn Route>>;
}

/// Two routers claiming the same method and path.
///
/// Reported rather than resolved: silently letting one router shadow another
/// would make which library answers a call depend on mount order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Collision {
    pub method: Method,
    pub path: &'static str,
}

impl std::fmt::Display for Collision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?} {} is already routed", self.method, self.path)
    }
}

impl std::error::Error for Collision {}

/// Why a call was not served.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unserved {
    /// No mounted route answers on that method and path.
    Unrouted,
    /// The route exists and the caller may not reach it.
    Denied(Denial),
}

/// Every route one socket serves.
#[derive(Default)]
pub struct Routes {
    mounted: Vec<Box<dyn Route>>,
}

impl Routes {
    /// A server with nothing mounted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many routes are mounted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mounted.len()
    }

    /// Whether nothing is mounted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mounted.is_empty()
    }

    /// Mounts a library's routes.
    ///
    /// # Errors
    /// Returns `Collision` when a route would shadow one already mounted. The
    /// merge stops there, so composition fails loudly at start-up rather than
    /// serving whichever library happened to be mounted first.
    pub fn mount(&mut self, router: &dyn Router) -> Result<(), Collision> {
        for route in router.routes() {
            if self.claimed(route.method(), route.path()) {
                return Err(Collision {
                    method: route.method(),
                    path: route.path(),
                });
            }
            self.mounted.push(route);
        }
        Ok(())
    }

    /// Serves one call: finds the route, checks its capability, runs it.
    ///
    /// # Errors
    /// Returns `Unserved::Unrouted` when nothing answers, and
    /// `Unserved::Denied` when the caller does not hold what the route requires.
    pub async fn dispatch(
        &self,
        method: Method,
        path: &str,
        mut call: Call,
        authority: &Authority,
    ) -> Result<Answer, Unserved> {
        let matched = self
            .mounted
            .iter()
            .find_map(|route| Self::captured(route.as_ref(), method, path).map(|arguments| (route, arguments)));
        let Some((route, arguments)) = matched else {
            return Err(Unserved::Unrouted);
        };
        let permit = authority.admit(route.requirement()).map_err(Unserved::Denied)?;
        call.arguments = arguments;
        Ok(route.call(&call, permit).await)
    }

    fn claimed(&self, method: Method, path: &str) -> bool {
        self.mounted
            .iter()
            .any(|route| route.method() == method && route.path() == path)
    }

    /// The arguments a route's pattern captures from a path, or `None` when the
    /// route does not answer for it.
    fn captured(route: &dyn Route, method: Method, path: &str) -> Option<BTreeMap<String, String>> {
        if route.method() != method {
            return None;
        }
        let pattern = route.path().split('/');
        let actual = path.split('/');
        if pattern.clone().count() != actual.clone().count() {
            return None;
        }
        pattern.zip(actual).try_fold(BTreeMap::new(), Self::segment)
    }

    fn segment(mut captured: BTreeMap<String, String>, pair: (&str, &str)) -> Option<BTreeMap<String, String>> {
        let (pattern, actual) = pair;
        let name = pattern.strip_prefix('{').and_then(|rest| rest.strip_suffix('}'));
        match name {
            Some(name) if !actual.is_empty() => {
                captured.insert(name.to_owned(), actual.to_owned());
                Some(captured)
            }
            Some(_) => None,
            None if pattern == actual => Some(captured),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, Call, Collision, Method, Outcome, Route, Router, Routes, Unserved};
    use crate::authority::{Authority, Permit};
    use crate::capability::{Capability, CapabilityKey, Grant};
    use crate::name::PeerName;

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Reach {
        Read,
        Control,
    }

    impl Capability for Reach {
        const DOMAIN: &'static str = "sample";
        const ALL: &'static [Self] = &[Self::Read, Self::Control];

        fn name(&self) -> &'static str {
            match self {
                Self::Read => "read",
                Self::Control => "control",
            }
        }
    }

    struct Listing;

    #[async_trait::async_trait]
    impl Route for Listing {
        fn method(&self) -> Method {
            Method::Get
        }

        fn path(&self) -> &'static str {
            "/containers/json"
        }

        fn requirement(&self) -> CapabilityKey {
            Reach::Read.key()
        }

        async fn call(&self, _call: &Call, _permit: Permit<'_, Authority>) -> Answer {
            Answer::done(b"[]".to_vec())
        }
    }

    struct Start;

    #[async_trait::async_trait]
    impl Route for Start {
        fn method(&self) -> Method {
            Method::Post
        }

        fn path(&self) -> &'static str {
            "/containers/{id}/start"
        }

        fn requirement(&self) -> CapabilityKey {
            Reach::Control.key()
        }

        async fn call(&self, call: &Call, _permit: Permit<'_, Authority>) -> Answer {
            Answer::done(call.arguments["id"].clone().into_bytes())
        }
    }

    struct Containers;

    impl Router for Containers {
        fn routes(&self) -> Vec<Box<dyn Route>> {
            vec![Box::new(Listing), Box::new(Start)]
        }
    }

    fn authority(capabilities: &[Reach]) -> Authority {
        Authority::new(
            PeerName::new("sample").expect("name"),
            Grant::new(capabilities.iter().copied()),
            Vec::new(),
        )
    }

    fn routes() -> Routes {
        let mut routes = Routes::new();
        routes.mount(&Containers).expect("mounted");
        routes
    }

    #[tokio::test]
    async fn a_route_answers_and_captures_its_path_arguments() {
        let answer = routes()
            .dispatch(
                Method::Post,
                "/containers/c1/start",
                Call::default(),
                &authority(&[Reach::Control]),
            )
            .await
            .expect("served");

        assert_eq!(answer.outcome, Outcome::Done);
        assert_eq!(answer.body, b"c1".to_vec());
    }

    #[tokio::test]
    async fn a_route_is_refused_without_the_capability_it_declares() {
        let refused = routes()
            .dispatch(
                Method::Post,
                "/containers/c1/start",
                Call::default(),
                &authority(&[Reach::Read]),
            )
            .await
            .expect_err("refused");

        let Unserved::Denied(denial) = refused else {
            panic!("a held capability must not be implied by another");
        };
        assert_eq!(denial.capability, Reach::Control.key());
    }

    #[tokio::test]
    async fn an_unrouted_path_is_distinct_from_a_refusal() {
        let unserved = routes()
            .dispatch(Method::Get, "/images/json", Call::default(), &authority(&[Reach::Read]))
            .await
            .expect_err("unrouted");
        assert_eq!(unserved, Unserved::Unrouted);

        let wrong_method = routes()
            .dispatch(
                Method::Delete,
                "/containers/json",
                Call::default(),
                &authority(&[Reach::Read]),
            )
            .await
            .expect_err("unrouted");
        assert_eq!(wrong_method, Unserved::Unrouted);
    }

    #[test]
    fn a_second_router_claiming_a_route_is_refused_rather_than_shadowing_it() {
        let mut routes = routes();
        let collision = routes.mount(&Containers).expect_err("refused");

        assert_eq!(
            collision,
            Collision {
                method: Method::Get,
                path: "/containers/json"
            }
        );
        assert_eq!(routes.len(), 2, "a refused merge mounts nothing further");
    }
}
