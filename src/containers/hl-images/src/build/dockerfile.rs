//! Typed Dockerfile syntax and parsing.

use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Clone, Debug, Eq, PartialEq)]
/// A parsed sequence of Dockerfile instructions.
pub struct Dockerfile {
    instructions: Vec<Instruction>,
}

impl Dockerfile {
    /// Transfers the parsed instruction sequence to a build driver.
    pub fn into_instructions(self) -> Vec<Instruction> {
        self.instructions
    }
}

impl FromStr for Dockerfile {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let escape = Self::escape(source)?;
        let mut instructions = Vec::new();
        let mut continuation = String::new();

        for (index, line) in source.lines().enumerate() {
            let line = line.trim_end();
            let trimmed = line.trim_start();
            if continuation.is_empty() && (trimmed.is_empty() || trimmed.starts_with('#')) {
                continue;
            }
            if let Some(part) = line.strip_suffix(escape) {
                continuation.push_str(part.trim_start());
                continuation.push(' ');
                continue;
            }
            continuation.push_str(trimmed);
            let text = continuation.trim();
            let Some((name, arguments)) = text.split_once(char::is_whitespace) else {
                return Err(format!(
                    "line {}: instruction requires arguments",
                    index + 1
                ));
            };
            instructions.push(Instruction {
                name: name.to_ascii_uppercase(),
                arguments: arguments.trim().to_owned(),
            });
            continuation.clear();
        }
        if !continuation.is_empty() {
            return Err("Dockerfile ends with an unfinished line continuation".to_owned());
        }
        Ok(Self { instructions })
    }
}

impl Dockerfile {
    fn escape(source: &str) -> Result<char, String> {
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some(comment) = line.strip_prefix('#') else {
                break;
            };
            let Some((name, value)) = comment.split_once('=') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("escape") {
                return match value.trim() {
                    "`" => Ok('`'),
                    "\\" => Ok('\\'),
                    value => Err(format!("invalid escape directive: {value}")),
                };
            }
        }
        Ok('\\')
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One normalized Dockerfile instruction.
pub struct Instruction {
    /// Uppercase instruction name.
    pub name: String,
    /// Unexpanded instruction arguments.
    pub arguments: String,
}

impl Instruction {
    /// Whether executing this instruction changes the root filesystem.
    pub fn mutates_filesystem(&self) -> bool {
        matches!(self.name.as_str(), "RUN" | "COPY" | "ADD" | "WORKDIR")
    }

    /// Expands variables in this instruction's arguments.
    pub fn substitute(&self, variables: &HashMap<String, String>) -> Result<String, String> {
        let source = self.arguments.as_str();
        if !source.contains('$') {
            return Ok(source.to_owned());
        }
        let mut output = String::with_capacity(source.len());
        let mut chars = source.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '$' {
                output.push(character);
                continue;
            }
            match chars.peek().copied() {
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for next in chars.by_ref() {
                        if next == '}' {
                            closed = true;
                            break;
                        }
                        name.push(next);
                    }
                    if !closed {
                        return Err("unterminated variable expansion".to_owned());
                    }
                    if let Some(value) = variables.get(&name) {
                        output.push_str(value);
                    }
                }
                Some(next) if next.is_ascii_alphanumeric() || next == '_' => {
                    let mut name = String::new();
                    while let Some(next) = chars.peek().copied() {
                        if next.is_ascii_alphanumeric() || next == '_' {
                            name.push(next);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if let Some(value) = variables.get(&name) {
                        output.push_str(value);
                    } else {
                        output.push('$');
                        output.push_str(&name);
                    }
                }
                _ => output.push('$'),
            }
        }
        Ok(output)
    }

    /// Parses `ENV` or `LABEL` arguments as modern or legacy key/value pairs.
    pub fn pairs(&self) -> Result<Vec<(String, String)>, String> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut quote = None;
        let mut present = false;
        let mut chars = self.arguments.chars();
        while let Some(character) = chars.next() {
            match character {
                '\\' => {
                    let next = chars.next().ok_or_else(|| "dangling escape".to_owned())?;
                    current.push(next);
                    present = true;
                }
                '"' | '\'' => match quote {
                    Some(open) if open == character => quote = None,
                    None => quote = Some(character),
                    Some(_) => current.push(character),
                },
                character if character.is_whitespace() && quote.is_none() => {
                    if present {
                        tokens.push(std::mem::take(&mut current));
                        present = false;
                    }
                }
                character => {
                    current.push(character);
                    present = true;
                }
            }
        }
        if quote.is_some() {
            return Err("unterminated quoted value".to_owned());
        }
        if present {
            tokens.push(current);
        }
        let Some(first) = tokens.first() else {
            return Err("missing key/value pair".to_owned());
        };
        if !first.contains('=') {
            if first.is_empty() {
                return Err("empty key".to_owned());
            }
            return Ok(vec![(first.clone(), tokens[1..].join(" "))]);
        }
        tokens
            .into_iter()
            .map(|token| {
                let (key, value) = token
                    .split_once('=')
                    .ok_or_else(|| format!("expected key=value: {token}"))?;
                if key.is_empty() {
                    return Err("empty key".to_owned());
                }
                Ok((key.to_owned(), value.to_owned()))
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The mutually exclusive exec and shell forms of a command instruction.
pub enum Command {
    /// A JSON-array argument vector.
    Exec(Vec<String>),
    /// Text interpreted by the configured shell.
    Shell(String),
}

impl FromStr for Command {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let source = source.trim();
        let json_form = source.starts_with('[')
            && !source
                .as_bytes()
                .get(1)
                .is_some_and(u8::is_ascii_whitespace);
        if !json_form {
            return Ok(Self::Shell(source.to_owned()));
        }
        let value: Value =
            serde_json::from_str(source).map_err(|error| format!("invalid exec form: {error}"))?;
        let Value::Array(values) = value else {
            return Err("exec form must be a JSON array".to_owned());
        };
        let arguments = values
            .into_iter()
            .map(|value| match value {
                Value::String(value) => Ok(value),
                other => Err(format!("exec form must contain only strings; got {other}")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::Exec(arguments))
    }
}

impl Command {
    /// Resolves this syntax form into the arguments passed to a runtime.
    pub fn resolve(&self, shell: &[String]) -> Result<Vec<String>, String> {
        match self {
            Self::Exec(arguments) => Ok(arguments.clone()),
            Self::Shell(command) if shell.is_empty() => {
                Err("shell form requires a configured shell".to_owned())
            }
            Self::Shell(command) => {
                let mut arguments = shell.to_vec();
                arguments.push(command.clone());
                Ok(arguments)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_parses_directive_and_continuation() {
        let parsed: Dockerfile = "# escape=`\nFROM ubuntu\nRUN echo a `\n && echo b\n"
            .parse()
            .unwrap();
        assert_eq!(parsed.instructions[1].arguments, "echo a  && echo b");
    }

    #[test]
    fn dockerfile_rejects_malformed_structure() {
        assert!(r"RUN echo \".parse::<Dockerfile>().is_err());
        assert!("# escape=x\nRUN x".parse::<Dockerfile>().is_err());
        assert!("FROM".parse::<Dockerfile>().is_err());
    }

    #[test]
    fn command_preserves_forms_and_rejects_malformed_exec() {
        assert_eq!(
            "[\"echo\",\"hi\"]".parse::<Command>().unwrap(),
            Command::Exec(vec!["echo".into(), "hi".into()])
        );
        assert_eq!(
            "echo hi".parse::<Command>().unwrap(),
            Command::Shell("echo hi".into())
        );
        assert_eq!(
            "[ -f /x ]".parse::<Command>().unwrap(),
            Command::Shell("[ -f /x ]".into())
        );
        assert!("[\"echo\", 1]".parse::<Command>().is_err());
        assert!("[not json]".parse::<Command>().is_err());
    }

    #[test]
    fn pairs_reject_malformed_values() {
        let instruction = Instruction {
            name: "ENV".into(),
            arguments: "A=1 B=2".into(),
        };
        assert_eq!(
            instruction.pairs().unwrap(),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        let malformed = Instruction {
            name: "LABEL".into(),
            arguments: "A=1 broken".into(),
        };
        assert!(malformed.pairs().is_err());
        let quote = Instruction {
            name: "ENV".into(),
            arguments: "A=\"no".into(),
        };
        assert!(quote.pairs().is_err());
    }

    #[test]
    fn substitution_rejects_unterminated_braces() {
        let instruction = Instruction {
            name: "RUN".into(),
            arguments: "echo ${NAME".into(),
        };
        assert!(instruction.substitute(&HashMap::new()).is_err());
        let mut variables = HashMap::new();
        variables.insert("NAME".into(), "value".into());
        assert!(instruction.substitute(&variables).is_err());
    }
}
