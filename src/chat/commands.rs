#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Model,
    Permissions,
    New,
    Status,
    Quit,
}

pub struct CommandSpec {
    pub command: Command,
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: [CommandSpec; 5] = [
    CommandSpec {
        command: Command::Model,
        name: "/model",
        description: "select model and reasoning level",
    },
    CommandSpec {
        command: Command::Permissions,
        name: "/permissions",
        description: "select what is allowed to do",
    },
    CommandSpec {
        command: Command::New,
        name: "/new",
        description: "start new conversation",
    },
    CommandSpec {
        command: Command::Status,
        name: "/status",
        description: "show session status",
    },
    CommandSpec {
        command: Command::Quit,
        name: "/quit",
        description: "exit",
    },
];

impl Command {
    pub fn parse(text: &str) -> Option<Self> {
        if text == "/exit" {
            return Some(Self::Quit);
        }
        COMMANDS
            .iter()
            .find(|entry| entry.name == text)
            .map(|entry| entry.command)
    }

    pub fn name(self) -> &'static str {
        COMMANDS
            .iter()
            .find(|entry| entry.command == self)
            .expect("registered command")
            .name
    }
}
