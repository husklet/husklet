//! The Linux `N_TTY` line discipline, as a pure function over a byte stream and a termios image.
//!
//! A guest running under Husklet gets a host pty, so on macOS a Linux guest runs the **BSD** line
//! discipline. The two disagree in a way that destroys data: BSD's `MAX_CANON` is 1024 and
//! overflowing it **flushes the entire input queue**, so a canonical line of 1025 bytes or more --
//! any pasted command of that length -- is silently discarded while every write reports success.
//! Linux allows 4096 and truncates instead, keeping the line. Measured on Linux: 281, 1024, 1025,
//! 1651 and 4001 bytes all arrive intact, and 5001 bytes arrives as exactly 4096 with the last byte
//! `\n`.
//!
//! Running the Linux discipline here rather than in the host kernel is what closes that gap. The
//! host slave is put in raw mode -- where a BSD pty applies backpressure instead of flushing, so the
//! channel underneath is lossless -- and this module supplies the discipline the guest expects.
//!
//! It is deliberately free of any host call. Input is `bytes in -> Effect out`, so every editing
//! key, the echo it generates and the 4096 rule are unit-testable with no pty, on any host.

/// Linux `N_TTY_BUF_SIZE`. A canonical line never grows past this, and the terminator overwrites the
/// last byte rather than extending it, which is what makes 5001 bytes of input arrive as 4096 ending
/// in the terminator instead of being thrown away.
pub(super) const CANONICAL_CAPACITY: usize = 4096;

/// `c_iflag` bits. These values are the asm-generic ones, which `aarch64` and `x86_64` share.
pub(super) mod input_flag {
    pub(super) const ISTRIP: u32 = 0x20;
    pub(super) const INLCR: u32 = 0x40;
    pub(super) const IGNCR: u32 = 0x80;
    pub(super) const ICRNL: u32 = 0x100;
    pub(super) const IXON: u32 = 0x400;
    pub(super) const IXANY: u32 = 0x800;
    pub(super) const IMAXBEL: u32 = 0x2000;
}

/// `c_oflag` bits.
pub(super) mod output_flag {
    pub(super) const OPOST: u32 = 0x1;
    pub(super) const ONLCR: u32 = 0x4;
    pub(super) const OCRNL: u32 = 0x8;
    pub(super) const ONLRET: u32 = 0x20;
}

/// `c_lflag` bits.
pub(super) mod local_flag {
    pub(super) const ISIG: u32 = 0x1;
    pub(super) const ICANON: u32 = 0x2;
    pub(super) const ECHO: u32 = 0x8;
    pub(super) const ECHOE: u32 = 0x10;
    pub(super) const ECHOK: u32 = 0x20;
    pub(super) const ECHONL: u32 = 0x40;
    pub(super) const NOFLSH: u32 = 0x80;
    pub(super) const ECHOCTL: u32 = 0x200;
    pub(super) const ECHOKE: u32 = 0x800;
    pub(super) const IEXTEN: u32 = 0x8000;
}

/// `c_cc` subscripts.
pub(super) mod control_character {
    pub(super) const VINTR: usize = 0;
    pub(super) const VQUIT: usize = 1;
    pub(super) const VERASE: usize = 2;
    pub(super) const VKILL: usize = 3;
    pub(super) const VEOF: usize = 4;
    pub(super) const VSTART: usize = 8;
    pub(super) const VSTOP: usize = 9;
    pub(super) const VSUSP: usize = 10;
    pub(super) const VEOL: usize = 11;
    pub(super) const VREPRINT: usize = 12;
    pub(super) const VWERASE: usize = 14;
    pub(super) const VLNEXT: usize = 15;
    pub(super) const VEOL2: usize = 16;
}

/// A guest's `struct termios`, as the 36-byte Linux image the engine records for each terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Termios {
    pub(super) input: u32,
    pub(super) output: u32,
    pub(super) control: u32,
    pub(super) local: u32,
    pub(super) line: u8,
    pub(super) characters: [u8; 19],
}

impl Termios {
    /// Decode the 36-byte image the engine hands back for a terminal.
    pub(super) fn from_image(image: &[u8; 36]) -> Self {
        let word = |at: usize| u32::from_ne_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]]);
        let mut characters = [0_u8; 19];
        characters.copy_from_slice(&image[17..36]);
        Self {
            input: word(0),
            output: word(4),
            control: word(8),
            local: word(12),
            line: image[16],
            characters,
        }
    }

    fn has_input(self, bit: u32) -> bool {
        self.input & bit != 0
    }

    fn has_output(self, bit: u32) -> bool {
        self.output & bit != 0
    }

    fn has_local(self, bit: u32) -> bool {
        self.local & bit != 0
    }

    /// A `c_cc` entry, or `None` when the guest disabled it with `_POSIX_VDISABLE`.
    fn character(self, index: usize) -> Option<u8> {
        match self.characters[index] {
            0 => None,
            value => Some(value),
        }
    }

    fn matches(self, index: usize, byte: u8) -> bool {
        self.character(index) == Some(byte)
    }
}

/// A signal the discipline decided to raise. Delivery is the pump's job: the host pty knows the
/// foreground process group, so `tcgetpgrp` on the master plus `killpg` reaches the guest without
/// any guest-to-host pid translation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Signal {
    Interrupt,
    Quit,
    Suspend,
}

/// What one batch of input bytes produced.
#[derive(Default, PartialEq, Eq, Debug)]
pub(super) struct Effect {
    /// Bytes the guest's `read` should observe, already assembled into complete canonical lines.
    pub(super) to_guest: Vec<u8>,
    /// Bytes to display, already output-processed. Echo is generated here because a raw host slave
    /// no longer echoes anything.
    pub(super) echo: Vec<u8>,
    /// Signals to raise, in the order the input asked for them.
    pub(super) signals: Vec<Signal>,
    /// The guest's `read` should observe end-of-file: a canonical `VEOF` arrived on an empty line.
    pub(super) end_of_file: bool,
    /// Discard whatever input has not yet reached the guest.
    pub(super) flush_input: bool,
    /// `IXON` asked output to stop (`Some(true)`) or resume (`Some(false)`).
    pub(super) output_stopped: Option<bool>,
}

/// The discipline's own state: the line being edited and the flow-control and quoting latches.
pub(super) struct LineDiscipline {
    termios: Termios,
    line: Vec<u8>,
    /// Display columns each byte of `line` occupies, so ERASE can rub out exactly what it drew.
    widths: Vec<u8>,
    column: usize,
    literal_next: bool,
    output_stopped: bool,
}

impl LineDiscipline {
    pub(super) fn new(termios: Termios) -> Self {
        Self {
            termios,
            line: Vec::new(),
            widths: Vec::new(),
            column: 0,
            literal_next: false,
            output_stopped: false,
        }
    }

    /// Adopt a termios the guest installed.
    ///
    /// Leaving canonical mode delivers whatever was being edited, exactly as the kernel does: the
    /// bytes were already accepted, so discarding them here would reintroduce the loss this module
    /// exists to remove.
    pub(super) fn set_termios(&mut self, termios: Termios, effect: &mut Effect) {
        let was_canonical = self.termios.has_local(local_flag::ICANON);
        self.termios = termios;
        if was_canonical && !termios.has_local(local_flag::ICANON) {
            effect.to_guest.extend_from_slice(&self.line);
            self.reset_line();
        }
    }

    fn reset_line(&mut self) {
        self.line.clear();
        self.widths.clear();
    }

    /// Feed a batch of input bytes through the discipline.
    pub(super) fn receive(&mut self, bytes: &[u8]) -> Effect {
        let mut effect = Effect::default();
        for &byte in bytes {
            self.receive_one(byte, &mut effect);
        }
        effect
    }

    fn receive_one(&mut self, byte: u8, effect: &mut Effect) {
        let termios = self.termios;
        let mut value = byte;
        if termios.has_input(input_flag::ISTRIP) {
            value &= 0x7f;
        }

        if self.literal_next {
            self.literal_next = false;
            self.accept(value, effect);
            return;
        }

        // Carriage-return and newline mapping. A quoted byte skips this, which is why the branch
        // sits after the literal-next check rather than before it.
        if value == b'\r' {
            if termios.has_input(input_flag::IGNCR) {
                return;
            }
            if termios.has_input(input_flag::ICRNL) {
                value = b'\n';
            }
        } else if value == b'\n' && termios.has_input(input_flag::INLCR) {
            value = b'\r';
        }

        if termios.has_input(input_flag::IXON) && self.flow_control(value, effect) {
            return;
        }

        if termios.has_local(local_flag::ISIG) && self.signal(value, effect) {
            return;
        }

        if termios.has_local(local_flag::ICANON) {
            self.canonical(value, effect);
        } else {
            self.echo_byte(value, effect);
            effect.to_guest.push(value);
        }
    }

    /// `IXON`/`IXANY`. Returns true when the byte was consumed as flow control.
    fn flow_control(&mut self, value: u8, effect: &mut Effect) -> bool {
        let termios = self.termios;
        if termios.matches(control_character::VSTOP, value) {
            self.output_stopped = true;
            effect.output_stopped = Some(true);
            return true;
        }
        if termios.matches(control_character::VSTART, value) {
            self.output_stopped = false;
            effect.output_stopped = Some(false);
            return true;
        }
        // IXANY lets any byte restart output, and that byte is still processed normally.
        if self.output_stopped && termios.has_input(input_flag::IXANY) {
            self.output_stopped = false;
            effect.output_stopped = Some(false);
        }
        false
    }

    /// `ISIG`. Returns true when the byte was consumed as a signal character.
    fn signal(&mut self, value: u8, effect: &mut Effect) -> bool {
        let termios = self.termios;
        let raised = if termios.matches(control_character::VINTR, value) {
            Signal::Interrupt
        } else if termios.matches(control_character::VQUIT, value) {
            Signal::Quit
        } else if termios.matches(control_character::VSUSP, value) {
            Signal::Suspend
        } else {
            return false;
        };
        // The character is shown before the queue is discarded, which is what puts the familiar
        // `^C` on the screen.
        self.echo_byte(value, effect);
        if !termios.has_local(local_flag::NOFLSH) {
            self.reset_line();
            effect.flush_input = true;
        }
        effect.signals.push(raised);
        true
    }

    fn canonical(&mut self, value: u8, effect: &mut Effect) {
        let termios = self.termios;
        let extended = termios.has_local(local_flag::IEXTEN);

        if extended && termios.matches(control_character::VLNEXT, value) {
            self.literal_next = true;
            // The caret is the ECHOCTL rendering of the quoting character itself, so a terminal that
            // asked not to see control characters drawn does not get one here either.
            if termios.has_local(local_flag::ECHO) && termios.has_local(local_flag::ECHOCTL) {
                // Draw a caret and back over it, so the next byte lands where the caret was.
                effect.echo.extend_from_slice(b"^\x08");
            }
            return;
        }
        if termios.matches(control_character::VERASE, value) {
            self.erase_one(effect);
            return;
        }
        if termios.matches(control_character::VKILL, value) {
            self.kill_line(effect);
            return;
        }
        if extended && termios.matches(control_character::VWERASE, value) {
            self.erase_word(effect);
            return;
        }
        if extended && termios.matches(control_character::VREPRINT, value) {
            self.reprint(effect);
            return;
        }
        if termios.matches(control_character::VEOF, value) {
            // EOF is never echoed. On an empty line it is end-of-file; otherwise it delivers what
            // has been typed so far, with no terminator.
            if self.line.is_empty() {
                effect.end_of_file = true;
            } else {
                effect.to_guest.extend_from_slice(&self.line);
                self.reset_line();
            }
            return;
        }
        let terminator = value == b'\n'
            || termios.matches(control_character::VEOL, value)
            || (extended && termios.matches(control_character::VEOL2, value));
        if terminator {
            self.terminate(value, effect);
            return;
        }
        self.accept(value, effect);
    }

    /// Append an ordinary byte to the line being edited, honouring the 4096 rule.
    fn accept(&mut self, value: u8, effect: &mut Effect) {
        if !self.termios.has_local(local_flag::ICANON) {
            self.echo_byte(value, effect);
            effect.to_guest.push(value);
            return;
        }
        if self.line.len() >= CANONICAL_CAPACITY {
            // The line is full. Linux drops the excess and keeps what it has, so the command still
            // arrives; only the overflow is lost. BSD would throw the whole line away.
            if self.termios.has_input(input_flag::IMAXBEL) {
                effect.echo.push(0x07);
            }
            return;
        }
        let width = self.echo_byte(value, effect);
        self.line.push(value);
        self.widths.push(width);
    }

    /// Finish the line with `terminator` and hand it to the guest.
    fn terminate(&mut self, terminator: u8, effect: &mut Effect) {
        if self.line.len() >= CANONICAL_CAPACITY {
            // Overwrite the last byte rather than growing past the buffer. This is what makes 5001
            // bytes of input arrive as exactly 4096 ending in the terminator.
            self.line.pop();
            self.widths.pop();
        }
        let termios = self.termios;
        if termios.has_local(local_flag::ECHO) || (terminator == b'\n' && termios.has_local(local_flag::ECHONL)) {
            effect.echo.extend_from_slice(&self.output_bytes(&[terminator]));
        }
        self.column = 0;
        self.line.push(terminator);
        effect.to_guest.extend_from_slice(&self.line);
        self.reset_line();
    }

    fn erase_one(&mut self, effect: &mut Effect) {
        let Some(width) = self.widths.pop() else {
            return;
        };
        self.line.pop();
        self.rub_out(width, effect);
    }

    fn erase_word(&mut self, effect: &mut Effect) {
        // Back over trailing blanks, then over the word itself, exactly as WERASE does.
        while self.line.last().is_some_and(|byte| *byte == b' ' || *byte == b'\t') {
            self.erase_one(effect);
        }
        while self.line.last().is_some_and(|byte| *byte != b' ' && *byte != b'\t') {
            self.erase_one(effect);
        }
    }

    fn kill_line(&mut self, effect: &mut Effect) {
        let termios = self.termios;
        if termios.has_local(local_flag::ECHO)
            && termios.has_local(local_flag::ECHOKE)
            && termios.has_local(local_flag::ECHOE)
        {
            while !self.widths.is_empty() {
                self.erase_one(effect);
            }
            return;
        }
        self.reset_line();
        self.column = 0;
        if termios.has_local(local_flag::ECHO) && termios.has_local(local_flag::ECHOK) {
            effect.echo.extend_from_slice(&self.output_bytes(b"\n"));
        }
    }

    fn reprint(&mut self, effect: &mut Effect) {
        if !self.termios.has_local(local_flag::ECHO) {
            return;
        }
        effect.echo.extend_from_slice(&self.output_bytes(b"\n"));
        self.column = 0;
        let line = std::mem::take(&mut self.line);
        let widths = std::mem::take(&mut self.widths);
        for &byte in &line {
            let mut discard = Effect::default();
            self.echo_byte(byte, &mut discard);
            effect.echo.extend_from_slice(&discard.echo);
        }
        self.line = line;
        self.widths = widths;
    }

    /// Rub out `width` display columns.
    fn rub_out(&mut self, width: u8, effect: &mut Effect) {
        let termios = self.termios;
        if !termios.has_local(local_flag::ECHO) {
            return;
        }
        if termios.has_local(local_flag::ECHOE) {
            for _ in 0..width {
                effect.echo.extend_from_slice(b"\x08 \x08");
            }
            self.column = self.column.saturating_sub(usize::from(width));
        } else if let Some(erase) = termios.character(control_character::VERASE) {
            effect.echo.push(erase);
        }
    }

    /// Echo one input byte, returning the display columns it occupied.
    fn echo_byte(&mut self, value: u8, effect: &mut Effect) -> u8 {
        let termios = self.termios;
        if !termios.has_local(local_flag::ECHO) {
            // The width still matters: ERASE must rub out the right amount if ECHO is turned back
            // on mid-line, and a zero here would desynchronise the column.
            return self.width_of(value);
        }
        let width = self.width_of(value);
        match value {
            b'\n' | b'\r' => {
                effect.echo.extend_from_slice(&self.output_bytes(&[value]));
                self.column = 0;
            }
            b'\t' => {
                effect.echo.push(b'\t');
                self.column += usize::from(width);
            }
            control if control < 0x20 || control == 0x7f => {
                if termios.has_local(local_flag::ECHOCTL) {
                    effect.echo.push(b'^');
                    effect.echo.push(if control == 0x7f { b'?' } else { control + 0x40 });
                    self.column += 2;
                }
            }
            printable => {
                effect.echo.push(printable);
                self.column += 1;
            }
        }
        width
    }

    fn width_of(&self, value: u8) -> u8 {
        match value {
            b'\n' | b'\r' => 0,
            b'\t' => {
                let advance = 8 - (self.column % 8);
                u8::try_from(advance).unwrap_or(8)
            }
            control if control < 0x20 || control == 0x7f => {
                if self.termios.has_local(local_flag::ECHOCTL) {
                    2
                } else {
                    0
                }
            }
            _ => 1,
        }
    }

    /// The guest's `VEOF`, or `None` when the guest disabled it with `_POSIX_VDISABLE`.
    ///
    /// The pump needs the exact byte: a raw slave has no `VEOF`, so end-of-file is raised by flipping
    /// the slave to canonical for one byte and writing this character.
    pub(super) fn end_of_file_character(&self) -> Option<u8> {
        self.termios.character(control_character::VEOF)
    }

    /// Apply `OPOST` to guest output. A raw host slave no longer translates anything, so `\n` would
    /// reach the display without its carriage return and every line would stair-step.
    pub(super) fn output_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        let termios = self.termios;
        if !termios.has_output(output_flag::OPOST) {
            return bytes.to_vec();
        }
        let mut out = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            match byte {
                b'\n' if termios.has_output(output_flag::ONLCR) => out.extend_from_slice(b"\r\n"),
                b'\n' if termios.has_output(output_flag::ONLRET) => out.push(b'\r'),
                b'\r' if termios.has_output(output_flag::OCRNL) => out.push(b'\n'),
                other => out.push(other),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::control_character::{
        VEOF, VEOL, VEOL2, VERASE, VINTR, VKILL, VLNEXT, VQUIT, VREPRINT, VSTART, VSTOP, VSUSP, VWERASE,
    };
    use super::input_flag::{ICRNL, IGNCR, IMAXBEL, INLCR, ISTRIP, IXANY, IXON};
    use super::local_flag::{ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, ECHONL, ICANON, IEXTEN, ISIG, NOFLSH};
    use super::output_flag::{OCRNL, ONLCR, OPOST};
    use super::{CANONICAL_CAPACITY, Effect, LineDiscipline, Signal, Termios};

    /// A termios as a shell leaves one: what `stty sane` installs.
    fn cooked() -> Termios {
        let mut characters = [0_u8; 19];
        characters[VINTR] = 0x03;
        characters[VQUIT] = 0x1c;
        characters[VERASE] = 0x7f;
        characters[VKILL] = 0x15;
        characters[VEOF] = 0x04;
        characters[VSTART] = 0x11;
        characters[VSTOP] = 0x13;
        characters[VSUSP] = 0x1a;
        characters[VREPRINT] = 0x12;
        characters[VWERASE] = 0x17;
        characters[VLNEXT] = 0x16;
        Termios {
            input: ICRNL | IXON,
            output: OPOST | ONLCR,
            control: 0,
            local: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
            line: 0,
            characters,
        }
    }

    fn without(termios: Termios, bits: u32) -> Termios {
        Termios {
            local: termios.local & !bits,
            ..termios
        }
    }

    fn feed(discipline: &mut LineDiscipline, bytes: &[u8]) -> Effect {
        discipline.receive(bytes)
    }

    // ---- the defect this module exists to fix -------------------------------------------------

    /// BSD flushes the whole input queue when a canonical line passes 1024 bytes, so on macOS a
    /// pasted command of that length is silently destroyed. Linux keeps every one of these.
    #[test]
    fn a_canonical_line_survives_every_length_bsd_would_have_flushed() {
        for length in [281_usize, 1024, 1025, 1651, 4001] {
            let mut discipline = LineDiscipline::new(cooked());
            let typed = vec![b'x'; length];
            let effect = feed(&mut discipline, &typed);
            assert!(
                effect.to_guest.is_empty(),
                "{length} bytes: nothing is delivered before the line is terminated"
            );
            let effect = feed(&mut discipline, b"\n");
            assert_eq!(
                effect.to_guest.len(),
                length + 1,
                "{length} bytes plus a newline must arrive whole"
            );
            assert_eq!(effect.to_guest[..length], typed[..], "{length} bytes arrived altered");
            assert_eq!(effect.to_guest[length], b'\n', "{length} bytes lost its terminator");
        }
    }

    /// Genuine overflow truncates and keeps the line -- it does not discard it. Measured on Linux:
    /// 5001 bytes arrive as exactly 4096 with the last byte a newline.
    #[test]
    fn overflowing_a_canonical_line_truncates_and_keeps_it() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, &vec![b'y'; 5000]);
        let effect = feed(&mut discipline, b"\n");
        assert_eq!(effect.to_guest.len(), CANONICAL_CAPACITY);
        assert_eq!(*effect.to_guest.last().expect("a terminated line"), b'\n');
        assert!(
            effect.to_guest[..CANONICAL_CAPACITY - 1]
                .iter()
                .all(|byte| *byte == b'y')
        );
    }

    #[test]
    fn a_full_line_rings_the_bell_when_imaxbel_asked_for_one() {
        let termios = Termios {
            input: cooked().input | IMAXBEL,
            ..cooked()
        };
        let mut discipline = LineDiscipline::new(termios);
        feed(&mut discipline, &vec![b'z'; CANONICAL_CAPACITY]);
        let effect = feed(&mut discipline, b"z");
        assert_eq!(effect.echo, vec![0x07], "an overflowing byte must ring, not draw");
        assert!(effect.to_guest.is_empty());
    }

    // ---- canonical vs non-canonical ------------------------------------------------------------

    #[test]
    fn a_line_reaches_the_guest_only_once_it_is_terminated() {
        let mut discipline = LineDiscipline::new(cooked());
        assert!(feed(&mut discipline, b"ls -l").to_guest.is_empty());
        assert_eq!(feed(&mut discipline, b"\n").to_guest, b"ls -l\n");
    }

    #[test]
    fn non_canonical_input_passes_straight_through() {
        let mut discipline = LineDiscipline::new(without(cooked(), ICANON));
        assert_eq!(feed(&mut discipline, b"abc").to_guest, b"abc");
    }

    #[test]
    fn leaving_canonical_mode_delivers_the_line_already_typed() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"half");
        let mut effect = Effect::default();
        discipline.set_termios(without(cooked(), ICANON), &mut effect);
        assert_eq!(effect.to_guest, b"half", "bytes already accepted must not be discarded");
    }

    // ---- editing keys --------------------------------------------------------------------------

    #[test]
    fn erase_removes_one_byte_and_rubs_out_exactly_what_it_drew() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"ab");
        let effect = feed(&mut discipline, &[0x7f]);
        assert_eq!(effect.echo, b"\x08 \x08", "ECHOE draws backspace-space-backspace");
        assert_eq!(feed(&mut discipline, b"\n").to_guest, b"a\n");
    }

    #[test]
    fn erase_on_an_empty_line_does_nothing() {
        let mut discipline = LineDiscipline::new(cooked());
        let effect = feed(&mut discipline, &[0x7f]);
        assert_eq!(effect, Effect::default(), "there is nothing to rub out");
    }

    #[test]
    fn erase_rubs_out_both_columns_of_a_control_character() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, &[0x16, 0x02]); // LNEXT then ^B, so ^B is data
        let effect = feed(&mut discipline, &[0x7f]);
        assert_eq!(effect.echo, b"\x08 \x08\x08 \x08", "^B occupied two columns");
    }

    #[test]
    fn kill_discards_the_line_and_erases_it_when_echoke_is_set() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"abc");
        let effect = feed(&mut discipline, &[0x15]);
        assert_eq!(effect.echo, b"\x08 \x08\x08 \x08\x08 \x08");
        assert_eq!(feed(&mut discipline, b"z\n").to_guest, b"z\n");
    }

    #[test]
    fn kill_without_echoke_echoes_a_newline_instead() {
        let mut discipline = LineDiscipline::new(without(cooked(), ECHOKE));
        feed(&mut discipline, b"abc");
        let effect = feed(&mut discipline, &[0x15]);
        assert_eq!(
            effect.echo, b"\r\n",
            "ECHOK moves to a fresh line, ONLCR adds the return"
        );
        assert_eq!(feed(&mut discipline, b"z\n").to_guest, b"z\n");
    }

    #[test]
    fn word_erase_removes_trailing_blanks_and_then_one_word() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"one two  ");
        feed(&mut discipline, &[0x17]);
        assert_eq!(feed(&mut discipline, b"\n").to_guest, b"one \n");
    }

    #[test]
    fn reprint_redraws_the_line_without_changing_it() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"abc");
        let effect = feed(&mut discipline, &[0x12]);
        assert_eq!(effect.echo, b"\r\nabc");
        assert_eq!(
            feed(&mut discipline, b"\n").to_guest,
            b"abc\n",
            "reprint must not alter the line"
        );
    }

    #[test]
    fn literal_next_quotes_the_following_control_character() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, &[0x16, 0x03]); // LNEXT then what would be INTR
        let effect = feed(&mut discipline, b"\n");
        assert_eq!(effect.to_guest, [0x03, b'\n'], "the quoted byte is data, not a signal");
        assert!(effect.signals.is_empty());
    }

    #[test]
    fn literal_next_quotes_the_erase_character_too() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"a");
        feed(&mut discipline, &[0x16, 0x7f]);
        assert_eq!(feed(&mut discipline, b"\n").to_guest, [b'a', 0x7f, b'\n']);
    }

    #[test]
    fn a_quoted_carriage_return_is_not_mapped_to_a_newline() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, &[0x16, b'\r']);
        let effect = feed(&mut discipline, b"\n");
        assert_eq!(effect.to_guest, [b'\r', b'\n'], "ICRNL must not reach a quoted byte");
    }

    // ---- end of file ---------------------------------------------------------------------------

    #[test]
    fn end_of_file_on_an_empty_line_is_end_of_file() {
        let mut discipline = LineDiscipline::new(cooked());
        let effect = feed(&mut discipline, &[0x04]);
        assert!(effect.end_of_file, "^D on an empty line is what ends `cat`");
        assert!(effect.to_guest.is_empty());
        assert!(effect.echo.is_empty(), "EOF is never echoed");
    }

    #[test]
    fn end_of_file_mid_line_delivers_the_line_without_a_terminator() {
        let mut discipline = LineDiscipline::new(cooked());
        feed(&mut discipline, b"partial");
        let effect = feed(&mut discipline, &[0x04]);
        assert_eq!(effect.to_guest, b"partial");
        assert!(
            !effect.end_of_file,
            "there were bytes to deliver, so this is not end of file"
        );
    }

    // ---- alternate terminators -----------------------------------------------------------------

    #[test]
    fn eol_and_eol2_terminate_a_line_like_a_newline() {
        for (index, terminator) in [(VEOL, 0x00_u8), (VEOL2, 0x00)] {
            let mut characters = cooked().characters;
            characters[index] = b';';
            let _ = terminator;
            let mut discipline = LineDiscipline::new(Termios { characters, ..cooked() });
            let effect = feed(&mut discipline, b"a;");
            assert_eq!(effect.to_guest, b"a;", "c_cc[{index}] must terminate the line");
        }
    }

    // ---- signals -------------------------------------------------------------------------------

    #[test]
    fn the_signal_characters_raise_their_signals_and_flush() {
        for (byte, expected) in [
            (0x03_u8, Signal::Interrupt),
            (0x1c, Signal::Quit),
            (0x1a, Signal::Suspend),
        ] {
            let mut discipline = LineDiscipline::new(cooked());
            feed(&mut discipline, b"typed");
            let effect = feed(&mut discipline, &[byte]);
            assert_eq!(effect.signals, vec![expected]);
            assert!(effect.flush_input, "a signal discards pending input unless NOFLSH");
            assert!(feed(&mut discipline, b"\n").to_guest == b"\n", "the line was discarded");
        }
    }

    #[test]
    fn an_interrupt_echoes_the_caret_form_when_echoctl_is_set() {
        let mut discipline = LineDiscipline::new(cooked());
        assert_eq!(feed(&mut discipline, &[0x03]).echo, b"^C");
    }

    #[test]
    fn noflsh_keeps_the_pending_line_across_a_signal() {
        let mut discipline = LineDiscipline::new(Termios {
            local: cooked().local | NOFLSH,
            ..cooked()
        });
        feed(&mut discipline, b"kept");
        let effect = feed(&mut discipline, &[0x03]);
        assert_eq!(effect.signals, vec![Signal::Interrupt]);
        assert!(!effect.flush_input);
        assert_eq!(feed(&mut discipline, b"\n").to_guest, b"kept\n");
    }

    #[test]
    fn clearing_isig_makes_the_signal_characters_ordinary_data() {
        let mut discipline = LineDiscipline::new(without(cooked(), ISIG));
        let effect = feed(&mut discipline, &[0x03]);
        assert!(effect.signals.is_empty());
        assert_eq!(feed(&mut discipline, b"\n").to_guest, [0x03, b'\n']);
    }

    // ---- flow control --------------------------------------------------------------------------

    #[test]
    fn ixon_stops_and_starts_output_and_consumes_the_characters() {
        let mut discipline = LineDiscipline::new(cooked());
        let stop = feed(&mut discipline, &[0x13]);
        assert_eq!(stop.output_stopped, Some(true));
        assert!(stop.to_guest.is_empty(), "^S is consumed, not delivered");
        let start = feed(&mut discipline, &[0x11]);
        assert_eq!(start.output_stopped, Some(false));
        assert!(start.to_guest.is_empty());
    }

    #[test]
    fn ixany_lets_any_byte_restart_output_and_still_be_typed() {
        let mut discipline = LineDiscipline::new(Termios {
            input: cooked().input | IXANY,
            ..cooked()
        });
        feed(&mut discipline, &[0x13]);
        let effect = feed(&mut discipline, b"k");
        assert_eq!(effect.output_stopped, Some(false));
        assert_eq!(
            feed(&mut discipline, b"\n").to_guest,
            b"k\n",
            "the restarting byte is still data"
        );
    }

    #[test]
    fn clearing_ixon_makes_the_flow_characters_ordinary_data() {
        let mut discipline = LineDiscipline::new(Termios {
            input: cooked().input & !IXON,
            ..cooked()
        });
        let effect = feed(&mut discipline, &[0x13]);
        assert_eq!(effect.output_stopped, None);
        assert_eq!(feed(&mut discipline, b"\n").to_guest, [0x13, b'\n']);
    }

    // ---- input mapping -------------------------------------------------------------------------

    #[test]
    fn icrnl_turns_a_carriage_return_into_the_line_terminator() {
        let mut discipline = LineDiscipline::new(cooked());
        assert_eq!(feed(&mut discipline, b"a\r").to_guest, b"a\n");
    }

    #[test]
    fn igncr_drops_a_carriage_return_entirely() {
        let mut discipline = LineDiscipline::new(Termios {
            input: (cooked().input & !ICRNL) | IGNCR,
            ..cooked()
        });
        let effect = feed(&mut discipline, b"a\r");
        assert!(effect.to_guest.is_empty(), "IGNCR discards the return");
        assert_eq!(feed(&mut discipline, b"\n").to_guest, b"a\n");
    }

    #[test]
    fn inlcr_turns_a_newline_into_a_carriage_return() {
        let mut discipline = LineDiscipline::new(Termios {
            input: (cooked().input & !ICRNL) | INLCR,
            ..cooked()
        });
        let effect = feed(&mut discipline, b"a\n");
        assert!(
            effect.to_guest.is_empty(),
            "the newline became a return, which does not terminate"
        );
    }

    #[test]
    fn istrip_clears_the_eighth_bit() {
        let mut discipline = LineDiscipline::new(Termios {
            input: cooked().input | ISTRIP,
            ..cooked()
        });
        assert_eq!(feed(&mut discipline, &[0xe1, b'\n']).to_guest, b"a\n");
    }

    // ---- echo ----------------------------------------------------------------------------------

    #[test]
    fn clearing_echo_draws_nothing_but_still_accepts_the_line() {
        let mut discipline = LineDiscipline::new(without(cooked(), ECHO));
        let effect = feed(&mut discipline, b"secret");
        assert!(effect.echo.is_empty(), "a password prompt must not draw what is typed");
        assert_eq!(feed(&mut discipline, b"\n").to_guest, b"secret\n");
    }

    #[test]
    fn echonl_shows_the_terminator_even_when_echo_is_off() {
        let mut discipline = LineDiscipline::new(Termios {
            local: (cooked().local & !ECHO) | ECHONL,
            ..cooked()
        });
        let effect = feed(&mut discipline, b"secret\n");
        assert_eq!(effect.echo, b"\r\n", "ECHONL draws the newline and nothing else");
    }

    #[test]
    fn clearing_echoctl_leaves_control_characters_undrawn() {
        let mut discipline = LineDiscipline::new(without(cooked(), ECHOCTL));
        let effect = feed(&mut discipline, &[0x16, 0x02]);
        assert!(
            effect.echo.iter().all(|byte| *byte != b'^'),
            "no caret form without ECHOCTL"
        );
    }

    #[test]
    fn a_tab_echoes_as_a_tab_and_erases_by_its_full_width() {
        let mut discipline = LineDiscipline::new(cooked());
        let effect = feed(&mut discipline, b"\t");
        assert_eq!(effect.echo, b"\t");
        let erased = feed(&mut discipline, &[0x7f]);
        assert_eq!(erased.echo.len(), 8 * 3, "a tab from column 0 spans eight columns");
    }

    // ---- output ----------------------------------------------------------------------------------

    #[test]
    fn opost_and_onlcr_add_the_carriage_return_a_raw_slave_no_longer_supplies() {
        let discipline = LineDiscipline::new(cooked());
        assert_eq!(discipline.output_bytes(b"one\ntwo\n"), b"one\r\ntwo\r\n");
    }

    #[test]
    fn clearing_opost_passes_output_through_untouched() {
        let discipline = LineDiscipline::new(Termios { output: 0, ..cooked() });
        assert_eq!(discipline.output_bytes(b"one\ntwo"), b"one\ntwo");
    }

    #[test]
    fn ocrnl_turns_an_output_return_into_a_newline() {
        let discipline = LineDiscipline::new(Termios {
            output: OPOST | OCRNL,
            ..cooked()
        });
        assert_eq!(discipline.output_bytes(b"a\rb"), b"a\nb");
    }

    // ---- the termios image -----------------------------------------------------------------------

    #[test]
    fn a_termios_image_decodes_the_fields_the_discipline_reads() {
        let mut image = [0_u8; 36];
        image[0..4].copy_from_slice(&(ICRNL | IXON).to_ne_bytes());
        image[4..8].copy_from_slice(&(OPOST | ONLCR).to_ne_bytes());
        image[12..16].copy_from_slice(&(ICANON | ECHO).to_ne_bytes());
        image[17 + VERASE] = 0x7f;
        image[17 + VEOF] = 0x04;
        let termios = Termios::from_image(&image);
        assert_eq!(termios.input, ICRNL | IXON);
        assert_eq!(termios.output, OPOST | ONLCR);
        assert_eq!(termios.local, ICANON | ECHO);
        assert_eq!(termios.characters[VERASE], 0x7f);
        assert_eq!(termios.characters[VEOF], 0x04);
    }

    #[test]
    fn a_disabled_control_character_never_matches() {
        let mut characters = cooked().characters;
        characters[VERASE] = 0; // _POSIX_VDISABLE
        let mut discipline = LineDiscipline::new(Termios { characters, ..cooked() });
        feed(&mut discipline, b"a");
        let effect = feed(&mut discipline, &[0x00]);
        assert!(effect.echo.is_empty() || !effect.echo.starts_with(b"\x08"));
        assert_eq!(feed(&mut discipline, b"\n").to_guest, [b'a', 0x00, b'\n']);
    }
}
