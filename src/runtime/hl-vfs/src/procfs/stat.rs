/// Linux task state encoded in field 3 of `/proc/<pid>/stat`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Running,
    Sleeping,
    DiskSleep,
    Zombie,
    Stopped,
    TracingStop,
    Dead,
    Wakekill,
    Waking,
    Parked,
    Idle,
}

impl State {
    const fn code(self) -> char {
        match self {
            Self::Running => 'R',
            Self::Sleeping => 'S',
            Self::DiskSleep => 'D',
            Self::Zombie => 'Z',
            Self::Stopped => 'T',
            Self::TracingStop => 't',
            Self::Dead => 'X',
            Self::Wakekill => 'K',
            Self::Waking => 'W',
            Self::Parked => 'P',
            Self::Idle => 'I',
        }
    }
}

/// Complete input for the positional 52-field Linux process-stat record.
///
/// There is deliberately no `Default`: a producer must obtain every task,
/// scheduler, clock, memory, signal, and image metric from its owning domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    pub process: u32,
    pub name: Vec<u8>,
    pub state: State,
    pub parent: u32,
    pub group: i32,
    pub session: i32,
    pub terminal: i32,
    pub foreground_group: i32,
    pub flags: u32,
    pub minor_faults: u64,
    pub child_minor_faults: u64,
    pub major_faults: u64,
    pub child_major_faults: u64,
    pub user_ticks: u64,
    pub system_ticks: u64,
    pub child_user_ticks: i64,
    pub child_system_ticks: i64,
    pub priority: i64,
    pub nice: i64,
    pub threads: i64,
    pub interval_ticks: i64,
    pub start_ticks: u64,
    pub virtual_bytes: u64,
    pub resident_pages: i64,
    pub resident_limit: u64,
    pub code_start: u64,
    pub code_end: u64,
    pub stack_start: u64,
    pub stack_pointer: u64,
    pub instruction_pointer: u64,
    pub pending_signals: u64,
    pub blocked_signals: u64,
    pub ignored_signals: u64,
    pub caught_signals: u64,
    pub wait_channel: u64,
    pub swapped_pages: u64,
    pub child_swapped_pages: u64,
    pub exit_signal: i32,
    pub processor: i32,
    pub realtime_priority: u32,
    pub policy: u32,
    pub delay_ticks: u64,
    pub guest_ticks: u64,
    pub child_guest_ticks: i64,
    pub data_start: u64,
    pub data_end: u64,
    pub heap_start: u64,
    pub arguments_start: u64,
    pub arguments_end: u64,
    pub environment_start: u64,
    pub environment_end: u64,
    pub exit_code: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Process,
    Name,
    Threads,
    Range,
}

/// Validated, bounded process-stat wire value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View(Input);

impl View {
    const MAX_NAME: usize = 15;
    const MAX_WIRE: usize = 2048;

    pub fn new(input: Input) -> Result<Self, Error> {
        if input.process == 0 {
            return Err(Error::Process);
        }
        if input.name.is_empty()
            || input.name.len() > Self::MAX_NAME
            || input.name.iter().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
        {
            return Err(Error::Name);
        }
        if input.threads <= 0 {
            return Err(Error::Threads);
        }
        if input.code_start > input.code_end
            || input.data_start > input.data_end
            || input.arguments_start > input.arguments_end
            || input.environment_start > input.environment_end
        {
            return Err(Error::Range);
        }
        Ok(Self(input))
    }

    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let value = &self.0;
        let mut output = format!("{} (", value.process).into_bytes();
        output.extend_from_slice(&value.name);
        output.extend_from_slice(format!(
            ") {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}\n",
            value.state.code(),
            value.parent,
            value.group,
            value.session,
            value.terminal,
            value.foreground_group,
            value.flags,
            value.minor_faults,
            value.child_minor_faults,
            value.major_faults,
            value.child_major_faults,
            value.user_ticks,
            value.system_ticks,
            value.child_user_ticks,
            value.child_system_ticks,
            value.priority,
            value.nice,
            value.threads,
            value.interval_ticks,
            value.start_ticks,
            value.virtual_bytes,
            value.resident_pages,
            value.resident_limit,
            value.code_start,
            value.code_end,
            value.stack_start,
            value.stack_pointer,
            value.instruction_pointer,
            value.pending_signals,
            value.blocked_signals,
            value.ignored_signals,
            value.caught_signals,
            value.wait_channel,
            value.swapped_pages,
            value.child_swapped_pages,
            value.exit_signal,
            value.processor,
            value.realtime_priority,
            value.policy,
            value.delay_ticks,
            value.guest_ticks,
            value.child_guest_ticks,
            value.data_start,
            value.data_end,
            value.heap_start,
            value.arguments_start,
            value.arguments_end,
            value.environment_start,
            value.environment_end,
            value.exit_code,
        ).as_bytes());
        debug_assert!(output.len() <= Self::MAX_WIRE);
        output
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn input() -> Input {
        Input {
            process: 7,
            name: b"worker name(x)".to_vec(),
            state: State::Sleeping,
            parent: 3,
            group: 4,
            session: 5,
            terminal: 0,
            foreground_group: -1,
            flags: u32::MAX,
            minor_faults: 6,
            child_minor_faults: 7,
            major_faults: 8,
            child_major_faults: 9,
            user_ticks: 10,
            system_ticks: 11,
            child_user_ticks: -12,
            child_system_ticks: -13,
            priority: -14,
            nice: -15,
            threads: 2,
            interval_ticks: -16,
            start_ticks: 17,
            virtual_bytes: u64::MAX,
            resident_pages: -18,
            resident_limit: u64::MAX,
            code_start: 19,
            code_end: 20,
            stack_start: 21,
            stack_pointer: 22,
            instruction_pointer: 23,
            pending_signals: 24,
            blocked_signals: 25,
            ignored_signals: 26,
            caught_signals: 27,
            wait_channel: 28,
            swapped_pages: 29,
            child_swapped_pages: 30,
            exit_signal: -31,
            processor: -32,
            realtime_priority: 33,
            policy: 34,
            delay_ticks: 35,
            guest_ticks: 36,
            child_guest_ticks: -37,
            data_start: 38,
            data_end: 39,
            heap_start: 40,
            arguments_start: 41,
            arguments_end: 42,
            environment_start: 43,
            environment_end: 44,
            exit_code: -45,
        }
    }

    #[test]
    fn exact_layout() {
        let bytes = View::new(input()).unwrap().bytes();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with("7 (worker name(x)) S 3 4 5 0 -1 4294967295 "));
        let close = text.rfind(") ").unwrap();
        let mut fields = text[close + 2..].split_ascii_whitespace().collect::<Vec<_>>();
        fields.insert(0, "(comm)");
        fields.insert(0, "7");
        assert_eq!(fields.len(), 52);
        assert_eq!(fields[2], "S");
        assert_eq!(fields[22], u64::MAX.to_string());
        assert_eq!(fields[23], "-18");
        assert_eq!(fields[37], "-31");
        assert_eq!(fields[43], "-37");
        assert_eq!(fields[51], "-45");
        assert!(bytes.len() <= View::MAX_WIRE);
    }

    #[test]
    fn construction_bounds() {
        let mut value = input();
        value.process = 0;
        assert!(matches!(View::new(value), Err(Error::Process)));
        let mut value = input();
        value.name = vec![b'x'; 16];
        assert!(matches!(View::new(value), Err(Error::Name)));
        let mut value = input();
        value.name = b"bad\nname".to_vec();
        assert!(matches!(View::new(value), Err(Error::Name)));
        let mut value = input();
        value.threads = 0;
        assert!(matches!(View::new(value), Err(Error::Threads)));
        let mut value = input();
        value.data_start = value.data_end + 1;
        assert!(matches!(View::new(value), Err(Error::Range)));
    }

    #[test]
    fn raw_comm() {
        let mut value = input();
        value.name = vec![b'a', 0xff, b')', b' ', b'b'];
        let bytes = View::new(value).unwrap().bytes();
        assert!(bytes.starts_with(&[b'7', b' ', b'(', b'a', 0xff, b')', b' ', b'b', b')', b' ', b'S']));
        assert_eq!(bytes.windows(2).filter(|bytes| *bytes == b") ").count(), 2);
    }
}
