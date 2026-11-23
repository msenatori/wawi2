//! Wawi 2's own tools, few and basic:
//!
//! - `recordar` / `olvidar`: the owner's facts in MEMORY.md.
//! - `avisame` / `recordatorios` / `cancelar_recordatorio`: timers and
//!   reminders, kept in `<home>/reminders.json` and said out loud by Wawi
//!   when due, through the core's floor.
//!
//! The date and time are not a tool: the host puts them in every turn
//! ([`Toolbox::now_line`]), which costs no round trip and is what the model
//! needs to fill `a_las`.
//!
//! Every tool answers on the spot (see [`tool::InstantTool`]). The clock is
//! injected ([`WallClock`]), so all of this runs in tests with a fake one.

pub mod clock;
pub mod memory;
pub mod reminders;
pub mod tool;

use std::cell::RefCell;
use std::rc::Rc;

use wawi_core::{Event, EventSink, Priority, Tool};
use wawi_memory::Home;

pub use clock::{Civil, FakeClock, RouteClock, WallClock};
pub use reminders::{Reminder, Reminders};
pub use tool::InstantTool;

/// The `source` of the floor requests that say reminders.
pub const FLOOR_SOURCE: &str = "recordatorio";

/// The tools and what they share: the clock, the memory folder and the
/// reminder list.
pub struct Toolbox {
    clock: Rc<dyn WallClock>,
    home: Option<Home>,
    reminders: reminders::Shared,
    /// Due while Wawi was closed; said once, at startup.
    missed: Vec<Reminder>,
    warning: Option<String>,
}

impl Toolbox {
    /// Reminders live in `<home>/reminders.json`; without a home they last
    /// only while the program runs, and there are no memory tools. Whatever
    /// came due while Wawi was closed is set aside for [`Self::missed_notice`].
    pub fn open(home: Option<Home>, clock: Rc<dyn WallClock>) -> Self {
        let (mut list, warning) = match &home {
            Some(home) => Reminders::open(home.root().join(reminders::FILE)),
            None => (Reminders::in_memory(), None),
        };
        let missed = list.take_due(clock.now());
        Self {
            clock,
            home,
            reminders: Rc::new(RefCell::new(list)),
            missed,
            warning,
        }
    }

    /// A problem with the reminders file found when opening, for the console.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn clock(&self) -> &Rc<dyn WallClock> {
        &self.clock
    }

    /// The tools, in the order to register them. Each reports its result
    /// through `sink`.
    pub fn tools(&self, sink: Rc<dyn EventSink>) -> Vec<Box<dyn Tool>> {
        self.instant_tools(sink)
            .into_iter()
            .map(|tool| Box::new(tool) as Box<dyn Tool>)
            .collect()
    }

    /// Runs one tool by name outside any turn, as the model would call it:
    /// the simulator's commands go through here, so they test the same code.
    /// `None` when there is no tool with that name.
    pub fn run(&self, name: &str, args_json: &str) -> Option<Result<String, String>> {
        self.instant_tools(Rc::new(Nowhere))
            .into_iter()
            .find(|tool| wawi_core::Tool::spec(tool).name == name)
            .map(|mut tool| tool.run(args_json))
    }

    fn instant_tools(&self, sink: Rc<dyn EventSink>) -> Vec<InstantTool> {
        let mut tools = Vec::new();
        if let Some(home) = &self.home {
            let clock = self.clock.clone();
            tools.push(memory::remember_tool(
                home.clone(),
                Box::new(move || clock.local_now().iso_date()),
                sink.clone(),
            ));
            tools.push(memory::forget_tool(home.clone(), sink.clone()));
        }
        tools.push(reminders::set_tool(
            self.reminders.clone(),
            self.clock.clone(),
            sink.clone(),
        ));
        tools.push(reminders::list_tool(
            self.reminders.clone(),
            self.clock.clone(),
            sink.clone(),
        ));
        tools.push(reminders::cancel_tool(
            self.reminders.clone(),
            self.clock.clone(),
            sink,
        ));
        tools
    }

    /// For `BrainContext::now`: the local date and time, in words and ISO.
    pub fn now_line(&self) -> String {
        clock::describe_now(&self.clock.local_now())
    }

    /// Today's local date, `YYYY-MM-DD`, for dating facts.
    pub fn today(&self) -> String {
        self.clock.local_now().iso_date()
    }

    /// What to say about the reminders missed while closed. Only once.
    pub fn missed_notice(&mut self) -> Option<Event> {
        let missed = std::mem::take(&mut self.missed);
        reminders::missed_notice(&missed, self.clock.now(), self.clock.as_ref()).map(floor)
    }

    /// A floor request for every reminder due now; they leave the list.
    pub fn due(&self) -> Vec<Event> {
        let now = self.clock.now();
        let due = self.reminders.borrow_mut().take_due(now);
        due.iter()
            .map(|reminder| floor(reminder.spoken(now, self.clock.as_ref())))
            .collect()
    }

    pub fn pending(&self) -> Vec<Reminder> {
        self.reminders.borrow().pending().to_vec()
    }

    /// The same as `recordar`, for the simulator's `/remember`.
    pub fn remember(&self, fact: &str) -> Result<String, String> {
        let home = self.home.as_ref().ok_or("sin memoria en esta sesión")?;
        memory::remember(home, fact, &self.today())
    }

    /// The same as `olvidar`, for the simulator's `/forget`.
    pub fn forget(&self, fact: &str) -> Result<String, String> {
        let home = self.home.as_ref().ok_or("sin memoria en esta sesión")?;
        memory::forget(home, fact)
    }
}

/// A sink for runs outside a turn: the caller takes the result directly.
struct Nowhere;

impl EventSink for Nowhere {
    fn send(&self, _event: Event) {}
}

/// Reminders matter, so they are `High`: never dropped, queued while the
/// owner talks or Wawi speaks.
fn floor(text: String) -> Event {
    Event::RequestFloor {
        priority: Priority::High,
        text,
        source: FLOOR_SOURCE.into(),
    }
}

#[cfg(test)]
mod tests;
