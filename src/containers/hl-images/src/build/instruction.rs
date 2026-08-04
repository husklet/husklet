use super::{Error, Result};

pub(super) struct Instruction {
    pub(super) name: String,
    pub(super) value: String,
}

pub(super) struct InstructionParser<'a> {
    lines: std::str::Lines<'a>,
    escape: char,
    started: bool,
}

impl<'a> InstructionParser<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self {
            lines: source.lines(),
            escape: '\\',
            started: false,
        }
    }

    pub(super) fn parse(mut self) -> Result<Vec<Instruction>> {
        let mut output = Vec::new();
        let mut line = String::new();
        let mut continued = false;
        for raw in self.lines.by_ref() {
            let raw = raw.trim_end();
            if line.is_empty() && raw.trim().is_empty() {
                continue;
            }
            if line.is_empty() && raw.trim_start().starts_with('#') {
                let comment = raw.trim_start().trim_start_matches('#').trim();
                if !self.started {
                    if let Some(value) = comment.strip_prefix("escape=") {
                        let mut characters = value.chars();
                        self.escape = match (characters.next(), characters.next()) {
                            (Some(value @ ('\\' | '`')), None) => value,
                            _ => {
                                return Err(Error::MalformedOci(
                                    "Dockerfile escape directive must be ` or backslash".into(),
                                ));
                            }
                        };
                    }
                }
                continue;
            }
            self.started = true;
            if let Some(part) = raw.strip_suffix(self.escape) {
                line.push_str(part.trim_start());
                line.push(' ');
                continued = true;
                continue;
            }
            line.push_str(raw.trim_start());
            let (name, value) = line
                .trim()
                .split_once(char::is_whitespace)
                .ok_or_else(|| Error::MalformedOci("Dockerfile instruction requires a value".into()))?;
            output.push(Instruction {
                name: name.to_ascii_uppercase(),
                value: value.trim().into(),
            });
            line.clear();
            continued = false;
        }
        if continued || !line.is_empty() {
            return Err(Error::MalformedOci("Dockerfile ends in a continuation".into()));
        }
        Ok(output)
    }
}

pub(super) struct Words<'a> {
    source: &'a str,
}

impl<'a> Words<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self { source }
    }

    pub(super) fn parse(&self) -> Vec<String> {
        self.tokenize(false).expect("permissive word parsing cannot fail")
    }

    pub(super) fn strict(&self) -> Result<Vec<String>> {
        self.tokenize(true)
    }

    fn tokenize(&self, strict: bool) -> Result<Vec<String>> {
        let mut output = Vec::new();
        let mut word = String::new();
        let mut quote = None;
        let mut escaped = false;
        for character in self.source.chars() {
            if escaped {
                word.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if matches!(character, '\'' | '"') {
                if quote == Some(character) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(character);
                } else {
                    word.push(character);
                }
            } else if character.is_whitespace() && quote.is_none() {
                if !word.is_empty() {
                    output.push(std::mem::take(&mut word));
                }
            } else {
                word.push(character);
            }
        }
        if escaped {
            word.push('\\');
        }
        if strict && quote.is_some() {
            return Err(Error::MalformedOci("unterminated quoted assignment".into()));
        }
        if !word.is_empty() {
            output.push(word);
        }
        Ok(output)
    }
}

pub(super) struct Assignments<'a> {
    source: &'a str,
}

impl<'a> Assignments<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self { source }
    }

    pub(super) fn parse(&self) -> Result<Vec<(String, String)>> {
        let values = Words::new(self.source).strict()?;
        if values.first().is_some_and(|value| value.contains('=')) {
            return values
                .into_iter()
                .map(|value| {
                    let (name, value) = value
                        .split_once('=')
                        .ok_or_else(|| Error::MalformedOci("entry must use NAME=VALUE".into()))?;
                    (!name.is_empty())
                        .then(|| (name.to_owned(), value.to_owned()))
                        .ok_or_else(|| Error::MalformedOci("name is empty".into()))
                })
                .collect();
        }
        if values.len() < 2 {
            return Err(Error::MalformedOci("entry requires a name and value".into()));
        }
        Ok(vec![(values[0].clone(), values[1..].join(" "))])
    }
}

pub(super) struct WorkingDirectory<'a> {
    current: &'a str,
}

impl<'a> WorkingDirectory<'a> {
    pub(super) fn new(current: &'a str) -> Self {
        Self { current }
    }

    pub(super) fn resolve(&self, value: &str) -> Result<String> {
        if value.is_empty() {
            return Err(Error::MalformedOci("invalid WORKDIR".into()));
        }
        let combined = if value.starts_with('/') {
            value.to_owned()
        } else {
            format!("{}/{value}", self.current.trim_end_matches('/'))
        };
        let mut components = Vec::new();
        for component in combined.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    components.pop();
                }
                component => components.push(component),
            }
        }
        Ok(format!("/{}", components.join("/")))
    }
}
