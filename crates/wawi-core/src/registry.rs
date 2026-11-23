use crate::plugin::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionLevel {
    Read,
    Write,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub params_schema_json: String,
    pub permission: PermissionLevel,
    pub needs_confirmation: bool,
    pub ack: Option<String>,
}

struct Entry {
    tool: Box<dyn Tool>,
    enabled: bool,
}

#[derive(Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

impl Registry {
    /// Registers a tool enabled. A tool with the same name is replaced.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Option<Box<dyn Tool>> {
        let name = tool.spec().name.clone();
        let previous = self.remove(&name);
        self.entries.push(Entry {
            tool,
            enabled: true,
        });
        previous
    }

    pub fn remove(&mut self, name: &str) -> Option<Box<dyn Tool>> {
        let index = self.position(name)?;
        Some(self.entries.remove(index).tool)
    }

    /// Returns false when no tool has that name.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> bool {
        match self.position(name) {
            Some(index) => {
                self.entries[index].enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// The spec of a tool that may run now, or the reason it may not.
    pub fn available(&self, name: &str) -> Result<&ToolSpec, String> {
        let entry = self
            .position(name)
            .map(|index| &self.entries[index])
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        if entry.enabled {
            Ok(entry.tool.spec())
        } else {
            Err(format!("disabled tool: {name}"))
        }
    }

    pub fn enabled_specs(&self) -> Vec<ToolSpec> {
        self.entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.tool.spec().clone())
            .collect()
    }

    pub fn tool_mut(&mut self, name: &str) -> Option<&mut (dyn Tool + 'static)> {
        let index = self.position(name)?;
        Some(self.entries[index].tool.as_mut())
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.tool.spec().name == name)
    }
}
