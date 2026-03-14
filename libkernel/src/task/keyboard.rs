use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;
use futures_util::stream::{Stream, StreamExt};
use futures_util::task::AtomicWaker;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use core::pin::Pin;
use core::task::{Context, Poll};
use crate::print;
use crate::println;

pub use pc_keyboard::DecodedKey as Key;

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    pub fn new() -> Self {
        SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(100))
            .expect("ScancodeStream::new should only be called once");
        ScancodeStream { _private: () }
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE.try_get().expect("not initialized");

        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        WAKER.register(&cx.waker());

        match queue.pop() {
            Some(scancode) => {
                WAKER.take();
                Poll::Ready(Some(scancode))
            },
            None => Poll::Pending,
        }
    }
}

/// A stream of fully-decoded keys, built on top of `ScancodeStream`.
/// Handles PS/2 scancode decoding internally; callers receive `Key` values.
pub struct KeyStream {
    scancodes: ScancodeStream,
    keyboard: Keyboard<layouts::Us104Key, ScancodeSet1>,
}

impl KeyStream {
    pub fn new() -> Self {
        KeyStream {
            scancodes: ScancodeStream::new(),
            keyboard: Keyboard::new(layouts::Us104Key, ScancodeSet1, HandleControl::Ignore),
        }
    }
}

impl Stream for KeyStream {
    type Item = Key;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Key>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.scancodes).poll_next(cx) {
                Poll::Ready(Some(scancode)) => {
                    if let Ok(Some(event)) = this.keyboard.add_byte(scancode) {
                        if let Some(key) = this.keyboard.process_keyevent(event) {
                            return Poll::Ready(Some(key));
                        }
                    }
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Called by the keyboard interrupt handler
///
/// Must not block or allocate.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if let Err(_) = queue.push(scancode) {
            println!("WARNING: scancode queue full; dropping keyboard input");
        } else {
            WAKER.wake();
        }
    } else {
        println!("WARNING: scancode queue uninitialized");
    }
}

pub async fn print_keypresses() {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(layouts::Us104Key, ScancodeSet1,
        HandleControl::Ignore);

    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(key_event) {
                match key {
                    DecodedKey::Unicode(character) => print!("{}", character),
                    DecodedKey::RawKey(key) => print!("{:?}", key),
                }
            }
        }
    }
}
