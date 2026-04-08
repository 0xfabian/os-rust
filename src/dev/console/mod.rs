mod font;
mod terminal;

use crate::boot::requests::FRAMEBUFFER_REQUEST;
use crate::sync::SpinLock;
use core::fmt::Write;
use terminal::Terminal;

static GLOBAL_TERMINAL: SpinLock<Option<Terminal>> = SpinLock::new(None);

pub fn init() -> bool {
    let fb = FRAMEBUFFER_REQUEST
        .get_response()
        .and_then(|resp| resp.framebuffers().next());

    let fb = match fb {
        Some(fb) => fb,
        None => return false,
    };

    let mut term = Terminal::new(&fb);
    term.clear();
    *GLOBAL_TERMINAL.lock() = Some(term);

    true
}

pub fn log(args: core::fmt::Arguments) {
    let mut terminal_guard = GLOBAL_TERMINAL.lock();

    if let Some(terminal) = &mut *terminal_guard {
        let _ = terminal.write_fmt(args);
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::dev::console::log(format_args!($($arg)*));
    };
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => {
        $crate::dev::console::log(format_args!("{}\n", format_args!($($arg)*)));
    };
}
