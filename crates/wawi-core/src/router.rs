use crate::text::normalize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteDecision {
    Handled(String),
    Pass,
}

pub trait Route {
    fn name(&self) -> &str;
    fn handle(&mut self, transcript: &str) -> RouteDecision;
}

/// Ordered routes, first match wins. When all pass, the transcript goes to the
/// brain, which is why the brain is not a route here.
#[derive(Default)]
pub struct Router {
    routes: Vec<Box<dyn Route>>,
}

impl Router {
    /// Appends a route, replacing any route with the same name.
    pub fn add(&mut self, route: Box<dyn Route>) {
        let name = route.name().to_owned();
        self.remove(&name);
        self.routes.push(route);
    }

    /// Inserts at `index` (clamped), replacing any route with the same name.
    pub fn insert(&mut self, index: usize, route: Box<dyn Route>) {
        let name = route.name().to_owned();
        self.remove(&name);
        let index = index.min(self.routes.len());
        self.routes.insert(index, route);
    }

    pub fn remove(&mut self, name: &str) -> Option<Box<dyn Route>> {
        let index = self.routes.iter().position(|r| r.name() == name)?;
        Some(self.routes.remove(index))
    }

    pub fn names(&self) -> Vec<&str> {
        self.routes.iter().map(|r| r.name()).collect()
    }

    /// Returns the name of the route that handled it, and its reply.
    pub fn route(&mut self, transcript: &str) -> Option<(String, String)> {
        for route in &mut self.routes {
            if let RouteDecision::Handled(reply) = route.handle(transcript) {
                return Some((route.name().to_owned(), reply));
            }
        }
        None
    }
}

pub trait Clock {
    /// Local hour and minute.
    fn now_hm(&self) -> (u8, u8);
}

pub struct TimeRoute<C> {
    clock: C,
}

impl<C: Clock> TimeRoute<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }
}

const TIME_QUESTIONS: &[&str] = &["que hora es", "que hora son", "what time is it"];

impl<C: Clock> Route for TimeRoute<C> {
    fn name(&self) -> &str {
        "time"
    }

    fn handle(&mut self, transcript: &str) -> RouteDecision {
        // Whole phrase only: "qué hora es en Tokio" must reach the brain.
        if TIME_QUESTIONS.contains(&normalize(transcript).as_str()) {
            let (hour, minute) = self.clock.now_hm();
            RouteDecision::Handled(format!("Son las {hour:02}:{minute:02}."))
        } else {
            RouteDecision::Pass
        }
    }
}
