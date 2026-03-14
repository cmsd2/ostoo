use futures_util::stream::StreamExt;
use libkernel::task::keyboard::{Key, KeyStream};
use libkernel::task::{executor, scheduler, timer};
use libkernel::{print, println};

const PROMPT: &str = "ostoo> ";
/// Maximum input characters; keeps typed text on a single VGA row.
const MAX_LINE: usize = 80 - 7 - 1; // 80 cols − len("ostoo> ") − safety margin

pub async fn run() {
    println!();
    print!("{}", PROMPT);

    let mut keys = KeyStream::new();
    let mut buf = [0u8; MAX_LINE];
    let mut len = 0usize;

    while let Some(key) = keys.next().await {
        match key {
            // Enter — run whatever is in the buffer
            Key::Unicode('\n') | Key::Unicode('\r') => {
                println!();
                let line = core::str::from_utf8(&buf[..len]).unwrap_or("").trim();
                execute(line);
                len = 0;
                print!("{}", PROMPT);
            }

            // Backspace
            Key::Unicode('\x08') => {
                if len > 0 {
                    len -= 1;
                    libkernel::vga_buffer::backspace();
                }
            }

            // Printable ASCII only
            Key::Unicode(c) if c.is_ascii() && !c.is_control() => {
                if len < MAX_LINE {
                    buf[len] = c as u8;
                    len += 1;
                    print!("{}", c);
                }
            }

            // Ignore raw keys (arrows, F-keys, etc.)
            _ => {}
        }
    }
}

fn execute(line: &str) {
    if line.is_empty() {
        return;
    }
    let (cmd, rest) = match line.find(' ') {
        Some(i) => (&line[..i], line[i + 1..].trim()),
        None => (line, ""),
    };
    match cmd {
        "help" => {
            println!("Commands:");
            println!("  help              show this message");
            println!("  clear             clear the screen");
            println!("  uptime            seconds since boot");
            println!("  tasks             ready / waiting task counts");
            println!("  threads           current thread and context-switch count");
            println!("  echo <text>       print text back");
        }
        "clear" => {
            libkernel::vga_buffer::clear_content();
        }
        "uptime" => {
            let secs = timer::ticks() / timer::TICKS_PER_SECOND;
            println!("uptime: {}s", secs);
        }
        "tasks" => {
            println!(
                "ready: {}  waiting: {}",
                executor::ready_count(),
                executor::wait_count()
            );
        }
        "threads" => {
            println!(
                "current thread: {}  context switches: {}",
                scheduler::current_thread_idx(),
                scheduler::context_switches()
            );
        }
        "echo" => {
            println!("{}", rest);
        }
        other => {
            println!("unknown command: '{}'  (try 'help')", other);
        }
    }
}
