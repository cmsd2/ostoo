//! Minimal VT100/ANSI escape sequence parser.
//!
//! Supports: newline, CR, BS, DEL, cursor home, clear screen, erase to EOL,
//! SGR colors (30-37/40-47, reset 0), cursor movement (A/B/C/D).

/// Default foreground: light grey (BGRA).
pub const DEFAULT_FG: u32 = 0x00AAAAAA;
/// Default background: black (BGRA).
pub const DEFAULT_BG: u32 = 0x00000000;

/// Parser state machine states.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Normal,
    Escape,   // saw ESC (0x1B)
    Csi,      // saw ESC [
    CsiParam, // collecting numeric parameters
}

/// Action emitted by the parser for each byte processed.
#[derive(Clone, Copy)]
pub enum Action {
    /// Print a character at current cursor position.
    Print(u8),
    /// Move cursor to next line, column 0.
    Newline,
    /// Move cursor to column 0.
    CarriageReturn,
    /// Move cursor left one column.
    Backspace,
    /// Move cursor to (0, 0).
    CursorHome,
    /// Clear entire screen.
    ClearScreen,
    /// Erase from cursor to end of line.
    EraseToEol,
    /// Move cursor up N rows.
    CursorUp(usize),
    /// Move cursor down N rows.
    CursorDown(usize),
    /// Move cursor right N columns.
    CursorRight(usize),
    /// Move cursor left N columns.
    CursorLeft(usize),
    /// Set foreground color (BGRA).
    SetFg(u32),
    /// Set background color (BGRA).
    SetBg(u32),
    /// Reset colors to default.
    ResetColors,
    /// No action (consumed by parser state machine).
    None,
}

pub struct Vt100Parser {
    state: State,
    params: [u16; 4],
    param_idx: usize,
}

impl Vt100Parser {
    pub fn new() -> Self {
        Vt100Parser {
            state: State::Normal,
            params: [0; 4],
            param_idx: 0,
        }
    }

    /// Feed one byte, returning the action to take.
    pub fn feed(&mut self, byte: u8) -> Action {
        match self.state {
            State::Normal => match byte {
                0x1B => {
                    self.state = State::Escape;
                    Action::None
                }
                b'\n' => Action::Newline,
                b'\r' => Action::CarriageReturn,
                0x08 => Action::Backspace,
                0x7F => Action::Backspace, // DEL
                0x20..=0x7E => Action::Print(byte),
                _ => Action::None,
            },
            State::Escape => {
                self.state = State::Normal;
                if byte == b'[' {
                    self.state = State::Csi;
                    self.params = [0; 4];
                    self.param_idx = 0;
                }
                Action::None
            }
            State::Csi | State::CsiParam => {
                if byte >= b'0' && byte <= b'9' {
                    self.state = State::CsiParam;
                    if self.param_idx < self.params.len() {
                        self.params[self.param_idx] =
                            self.params[self.param_idx].saturating_mul(10).saturating_add((byte - b'0') as u16);
                    }
                    Action::None
                } else if byte == b';' {
                    if self.param_idx < self.params.len() - 1 {
                        self.param_idx += 1;
                    }
                    Action::None
                } else {
                    // Final character — dispatch.
                    self.state = State::Normal;
                    self.dispatch_csi(byte)
                }
            }
        }
    }

    fn dispatch_csi(&self, cmd: u8) -> Action {
        let p0 = self.params[0] as usize;
        let n = if p0 == 0 { 1 } else { p0 };

        match cmd {
            b'A' => Action::CursorUp(n),
            b'B' => Action::CursorDown(n),
            b'C' => Action::CursorRight(n),
            b'D' => Action::CursorLeft(n),
            b'H' => Action::CursorHome,
            b'J' => {
                if p0 == 2 {
                    Action::ClearScreen
                } else {
                    Action::None
                }
            }
            b'K' => Action::EraseToEol,
            b'm' => self.dispatch_sgr(),
            _ => Action::None,
        }
    }

    fn dispatch_sgr(&self) -> Action {
        let p0 = self.params[0];
        match p0 {
            0 => Action::ResetColors,
            30..=37 => Action::SetFg(ansi_color(p0 - 30)),
            40..=47 => Action::SetBg(ansi_color(p0 - 40)),
            _ => Action::None,
        }
    }
}

/// Map ANSI color index (0–7) to BGRA value.
fn ansi_color(idx: u16) -> u32 {
    const COLORS: [u32; 8] = [
        0x00000000, // black
        0x000000AA, // red
        0x0000AA00, // green
        0x000055AA, // brown/yellow
        0x00AA0000, // blue
        0x00AA00AA, // magenta
        0x00AAAA00, // cyan
        0x00AAAAAA, // white/light grey
    ];
    COLORS[idx as usize & 7]
}
